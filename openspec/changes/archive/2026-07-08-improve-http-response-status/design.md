## Context

`/execute` already returns a fully-classified error envelope: every system error carries
`retryable: bool` and `owner: caller|developer|operator` (`crates/runlet-wire/src/errors.rs`,
surfaced as `ErrorEnvelope` in `crates/runlet-core/src/errors.rs`; documented in
`docs/99-errors.md`). The HTTP status, however, is assigned per-condition
(`EngineError::http_status()` in `engine.rs`, plus builders in `runlet/src/handler.rs`) and is
inconsistent with that classification in one large way: the entire `capability` bucket —
db/redis/amq/mail/s3/auth failures, including retryable ones — is forced to **HTTP 200**
(`EngineError::Capability` → 200). A queue worker routing on the status line therefore acks and
drops a message that failed on a transient dependency and would have succeeded on retry.

The primary consumer we are optimising for is dumb retry infrastructure (queue workers, service
meshes, generic HTTP retry middleware) that routes on the status *line* without parsing the body.
The service is pre-publish with no external consumers, so a breaking status change is cheap now.

## Goals / Non-Goals

**Goals:**
- Make the HTTP status a faithful projection of the envelope so `4xx = park`, `5xx = retry`,
  `2xx = ack` is *true* for `/execute`, including capability failures.
- Keep the envelope authoritative and unchanged on the wire — this is a projection, not a new
  classification. Envelope-readers get a status that never contradicts the body (the double win).
- One projection function as the single source of truth; no scattered status literals.
- Give operators a knob for the genuinely-ambiguous `TIMEOUT`/`MEMORY_LIMIT` case.

**Non-Goals:**
- Changing `/batch`: an admitted batch has per-item outcomes that cannot share one status line,
  so it stays `200`-with-envelope. Batch consumers are envelope-readers by construction.
- Changing `runlet-wire` types or the `Fault`/`retryable`/`owner` semantics.
- Adding an opt-in / content-negotiated dual path (`Prefer: outcome-status`). We considered it for
  compatibility but rejected it (see Decisions) — no consumers to protect, and a permanent dual
  path is a standing tax.
- Rewriting handler-returned error bodies (D1 stays: verbatim passthrough).

## Decisions

### D1 — Status class is a pure function of `(success, retryable)`; `owner` never changes the class

`success ⇒ 2xx`, `retryable=true ⇒ 5xx`, `retryable=false ⇒ 4xx`. `owner` only picks *which*
code inside the class and rides the body to route the fix. This is the whole design in one line,
and it dissolves the awkward "operator-owned but non-retryable" cell: such a misconfig is
`retryable=false ⇒ 4xx` (park), regardless of `owner`, because retrying it cannot help.

- *Alternative considered — status keyed on `owner`* (caller→4xx, developer→4xx, operator→5xx):
  rejected. It puts non-retryable operator misconfig at `5xx`, telling a worker to retry something
  that will never succeed until a human edits config. `retryable` is exactly the signal a worker
  needs; `owner` is for paging, not routing.

### D2 — Every retryable is `503` (except `INTERNAL` → `500`); no `429`; attach `Retry-After` (ratified)

- **Status**: approved
- Retryable capacity/quota (`OVERLOADED`, `PARTITION_OVERLOADED`, `QUOTA_EXCEEDED`) → **`503`**, *not* `429`;
  `INTERNAL` → `500`; every other transient failure (dependency outages, retryable capability codes,
  retryable timeout) → `503`. `503` (and `500`) carry `Retry-After`, seeded from the per-target
  circuit-breaker cool-down when one is open, else a configured default. `Retry-After` is the field a
  *truly* dumb backoff reads — the status says "retry", the header says "when".
- **Why**: `429` has a `4xx` *digit* but retry semantics — it directly violates "first digit = class."
  A naive worker doing `if 5xx: retry` would **park** a `429`, silently dropping a message that only
  needed to wait — the exact failure this change exists to kill. Purity beats the familiar rate-limit
  status; `Retry-After` still carries the backoff horizon (per-second rate-limit vs monthly hard-cap
  are distinguished by its *value*, not the code).
- **Considered**: keep `429` for capacity/quota (familiar to sophisticated clients) — rejected, its
  `4xx` digit lies to a one-digit worker. Considered per-cause `5xx` split beyond `500`/`503` —
  unnecessary; `Retry-After` carries the timing nuance.

### D3 — Capability errors project instead of forcing `200` (**BREAKING**, clean)

The top-of-request handling that maps `EngineError::Capability → 200` is replaced by the
projection. Ship as a clean break: no opt-in header, no alias, no dual path — pre-publish, no
consumers. A capability error the *handler catches* and re-returns is a handler-owned error and
follows D5, not this rule; only *uncaught* capability failures that become the request outcome
project here.

- *Alternative — Strategy C (opt-in via `Prefer: outcome-status`)*: preserves the old contract by
  default. Rejected: no consumers to preserve, and it would leave the default behaviour wrong
  (retryable outages acked as 200) plus a permanent two-path maintenance cost.

### D4 — Only `TIMEOUT` is knob-driven (default `true`); `MEMORY_LIMIT`/`max_ops` are always non-retryable (ratified)

- **Status**: approved
- New `config.timeout_retryable: bool`, default **`true`**, governs **only** `TIMEOUT`. A wall-clock
  timeout is genuinely ambiguous (a slow dependency vs a slow algorithm); the engine can't tell them
  apart. Default to retry (`503`): the retry ladder *bounds* a runaway (N tries then park), whereas a
  false-permanent parks a message that would have succeeded = data loss — the asymmetry favors
  retry. Operators with compute-heavy deterministic workloads flip it off (`false` → `422`).
- `MEMORY_LIMIT` and `max_ops` are **deterministic** for a given (script, input): the same request
  hits the same limit every time, so retry is pure waste. They stay **non-retryable → `422`**,
  independent of the knob. (This is where I diverged from the "default everything ambiguous to
  retryable" advice — these aren't ambiguous.)
- **Why (default flip `false`→`true`)**: queue-first framing — the whole change exists so workers
  don't drop recoverable messages; a park-by-default timeout reintroduces exactly that loss. The
  ladder makes the false-retryable cost bounded.
- **Considered**: default `false` (tenant blast-radius — a buggy tenant's infinite loop parks instead
  of retrying N×). Legitimate, but the knob exists precisely for that operator; the *default* follows
  the change's motivation. Considered lumping `MEMORY_LIMIT`/`max_ops` under the knob (the advice's
  bucketing) — rejected, they're deterministic. Considered hardcoding `504` — rejected, a
  deployment-specific bet baked into the platform.

### D5 — `200 ⟺ error === null`; handler-declared retryability is a single opt-in key; body stays verbatim (ratified)

- **Status**: approved
- **Purity of 200**: an error-present response is **never** `2xx`. This kills the `(success=true,
  error-present)` "200-with-success=false" case that a one-digit worker cannot route, and formally
  normalizes the nonsense `(success=true, retryable=true)` cell — `retryable` is only *read* when
  `error` is non-null. The engine computes the class; a script can never emit a `200` with an error.
- **Handler opt-in**: D1 (opaque passthrough) is preserved with one narrow read — if the returned
  `error` object carries a top-level boolean `retryable`, project it (`true ⇒ 503`, `false ⇒ 422`).
  An un-annotated handler error defaults to **`422` (park)**, *not* `200`.
- **Why the `422` default (not the advice's retryable-by-default)**: handler-authored errors are
  overwhelmingly *permanent* rejections (validation, not-found, business-rule). Defaulting them to
  `503` would create a retry storm — a malformed request hammered N× before parking. The
  safe-default-retryable rule is for *platform* faults the box couldn't classify, not for errors an
  author deliberately returned; here park is the safe common case, and the author opts *into* retry.
- The box **reads** the key to set the status line but never **rewrites** the body — verbatim in all
  cases (`503`, `422`).
- **Considered**: keep un-annotated → `200` (legacy "ran fine, read the body") — rejected, it's the
  200-with-error hole. Classify all handler errors — impossible without guessing business semantics.
  Read a richer object (code/owner) — scope creep; `retryable` is the one bit the status line needs.

### D6 — One projection function, single source of truth

Introduce a single `fn http_status(fault) -> (StatusCode, Option<RetryAfter>)` (name TBD in apply)
that all sites call, driven off `(retryable, owner, code-class)`. `EngineError::http_status()` and
the per-condition builders in `handler.rs` collapse into calls to it. This is what makes the
invariant enforceable rather than aspirational, and keeps `docs/99-errors.md`'s table a direct
render of one function.

### D7 — Non-contradiction is structural, not conventional (ratified)

- **Status**: approved
- `Fault` is the **only** constructor for a system error's `(code, retryable, owner)`; `code` is
  drawn from a registered catalog where each entry *carries* its `(retryable, owner)`. An author
  picks a code and the class **falls out** of the catalog entry — a status that contradicts the real
  class becomes *unrepresentable*, not merely discouraged. **No ad-hoc `Fault` literals or hand-rolled
  statuses anywhere** — any site that bypasses the catalog is exactly where the invariant would leak.
- **Why**: purity (D1) is only trustworthy if `code`/`owner`/`body` are *passengers* that can never
  become inputs to the routing digit. Enforcing that in the type system beats a review-time
  convention. `docs/99-errors.md`'s per-code tables *are* the catalog.
- **Isolation**: the catalog + `Fault::new` in `crates/runlet-wire/src/errors.rs`; the projection
  fn (D6) is the only consumer that turns a `Fault` into a status.

## Build-vs-Adopt gate

This change is a projection over runlet's own error taxonomy — nothing external speaks it, so the
hierarchy resolves to Rent-the-standard / Extend-existing, with one justified small Build.

### Decision: Retry signaling — Rent (HTTP standard)

- **Status**: approved
- **Why**: use `Retry-After` (RFC 9110) — universally honored by generic retry infra; no custom
  header a naive worker would ignore.
- **Considered**: custom `Runlet-Retryable` header (naive workers don't read it); RFC 9457
  `application/problem+json` for the error *body* (out of scope — this change touches the status
  line + one header, not the envelope body).
- **Isolation**: header emission in the response builder, off the projection fn's `Option<RetryAfter>`.

### Decision: Backoff timing source — Extend (existing `CircuitBreaker`)

- **Status**: approved
- **Why**: seed `Retry-After` from the existing `runlet-wire::CircuitBreaker` cool-down; a configured
  default when no breaker is open. No new backoff/rate-limit crate.
- **Considered**: a `backoff`/`governor` dependency — unneeded, the breaker already tracks cool-down.
- **Isolation**: the breaker already lives behind `runlet-wire`; the projection fn reads its cool-down.

### Decision: Status projection + non-contradiction — Build/Extend (hand-written over `Fault`)

- **Status**: approved
- **Why**: no external tool maps a bespoke `Fault` taxonomy to HTTP; it's one pure function (D6) plus
  the catalog/constructor hardening (D7) extending the existing `runlet-wire::Fault`.
- **Considered**: nothing adoptable exists for this contract; a generic error-mapping crate would add
  a dependency without expressing `(retryable, owner)` semantics.
- **Isolation**: `http_status(fault)` fn (sole authority) + `Fault`/catalog in `runlet-wire`.

## Risks / Trade-offs

- **[Breaking: capability errors move off 200]** → Acceptable: pre-publish, no external consumers
  (proposal + `CLAUDE.md` confirm single/no known consumer). Documented in `docs/99-errors.md` and
  the spec delta; `tests/test_simple.py` assertions updated in the same change.
- **[`timeout_retryable=true` could let a genuinely-slow script poison a queue]** → Mitigated by
  the conservative default (`false`) and by the operator opting in only when workloads are
  dependency-bound. The `max_ops`/wall-clock budget still bounds each attempt.
- **[Handler opt-in `retryable` collides with an app that already returns a `retryable` field for
  its own meaning]** → Low risk (pre-publish); the key is only *read* for status, never altered,
  so a mis-signal changes only the status line, not the body. Documented as reserved.
- **[Status/`owner` orthogonality is subtle]** → Mitigated by D1 stated as one rule and by the
  projection function centralising it; reviewers check one function, not N call sites.
- **[`Retry-After` seeding when no breaker is open]** → Falls back to a configured default; never
  emitted without a value.

## Migration Plan

1. Add `config.timeout_retryable` (default `false`); thread it into the `TIMEOUT`/`MEMORY_LIMIT`
   fault construction.
2. Add the single projection function; route `EngineError::http_status()` and the `handler.rs`
   builders through it. Remove the `Capability → 200` special case.
3. Emit `Retry-After` on `429`/`503`.
4. Read the opt-in `retryable` key in the handler-envelope parse path (`struct Envelope`) for the
   status decision only.
5. Update `docs/99-errors.md` (traffic-light + per-code tables) and `tests/test_simple.py`.
6. Rollback: revert the projection wiring; the wire envelope is unchanged, so no data migration.

## Open Questions

- Exact `Retry-After` default (seconds) when no circuit-breaker cool-down applies — pick in apply.
- Whether `409` is the best code for operator-non-retryable misconfig vs `424 Failed Dependency`;
  both are `4xx` (park), so worker behaviour is identical — cosmetic, decide in apply.
- Should `/batch` optionally surface an aggregate `Retry-After` when *every* item is retryable? Out
  of scope for this change; noted for a possible follow-up.
