# Design — diagnostic-log-channel

## Context

Runlet's sandbox exposes `json`, `emit`, `$`/`Decimal`, `$sys`, and the opt-in capability globals —
but **no `console`**. A handler author cannot see intermediate values; a failing run leaves no
trace of *why*. The box is never hit directly: it sits behind the **nexus gateway**, which asserts
trusted identity (tenant/user/plan), continues the W3C `traceparent`, and — new — will host a
**browser playground/editor** for customers to develop apps.

Two consumers therefore need diagnostics: **production apps** (structured operational logs that flow
to a tenant-scoped stream, asynchronously) and the **playground** (logs shown *inline* the moment
you hit Run). The infrastructure already exists for the first: `crates/runlet/src/events.rs` is a
per-tenant, `trace_id`-correlated, non-blocking (`try_send`, drop-on-full) event pipeline emitting a
versioned envelope to stdout — today carrying `usage` (billing) and `audit` (compliance) events, and
explicitly designed as "the seam a durable per-tenant outbox drops into later." A log is just a
third event kind. And the `emit-effects-channel` change (archived) already established the exact
machinery this reuses: a bounded per-invocation buffer drained after execution on both the success
and error paths, a public entry type threaded `ExecResult → Outcome`, surfaced at the edge.

This change adds `log.*` and routes it, by policy, to those two sinks.

## Goals / Non-Goals

**Goals:**
- A structured, leveled `log.*` primitive in the sandbox (Serilog templates + Pino levels/context).
- Always-on delivery to a per-tenant **stream** (the `events.rs` pipeline) as a new event kind, on
  its **own** bounded channel so it can never starve `usage`/`audit`.
- A gateway-gated **response mirror** (`logs[]`) with capture-on-failure, for the playground.
- Near-free when below the level floor; bounded so one run can't flood.
- Logs kept outside the bit-reproducible `data`/`effects` contract.

**Non-Goals:**
- Durable / guaranteed log delivery — logs stay lossy (observability-grade).
- Per-tenant routing to a customer-owned external sink (Fastly BYO-endpoint style).
- Log-based metrics; nestable tracing spans (the request is already one server-side OTel span).
- The playground UI (nexus owns it).
- `console.log` compatibility.

## Decisions

### D1 — `log.*`, not `console.log`
A dedicated `log` global with explicit levels (`trace/debug/info/warn/error`). **Why:** `console`
carries the mental model "prints to a stream you own," which a stateless box behind a gateway cannot
honor; a captured, policy-routed, sometimes-dropped sink deserves an honest name. *Alternative
rejected:* a `console` shim — familiar, but drags in `console.error/table/…` expectations and lies
about where output goes.

### D2 — Serilog message templates (template + properties + rendered), not a flat string
`log.info("charged {user} {amount}", { user, amount })` captures the **template**, the **named
properties**, and a **rendered message**. **Why:** you get a human-readable line *and* queryable
structured fields from one call; the playground's log panel can color by level and render
key/values, and a future indexed sink can filter on properties without reparsing strings. *Alt
rejected:* pre-rendered string only (loses structure, the thing that makes logs useful downstream).

### D3 — Multi-sink model: one event, sinks selected by policy, invisible to the script
The script calls `log.info(...)`; **policy** (config + the gateway's trusted flags) decides which
sinks receive it. **Why:** this is Serilog's/tracing's core idea and it cleanly composes the two
consumers — the same call streams in production and mirrors-to-response in the playground, with the
author never branching on environment. It also makes the response mirror *additive*: sinks can be
added/removed without touching the `log.*` API.

### D4 — Stream sink = a new `EventBody::Log` on its **own** bounded channel
Logs ride the existing `events.rs` pipeline as a new `log`-type event (tenant-keyed,
`trace_id`-correlated, non-blocking), but through a **separate bounded channel** from `usage`/`audit`
with its own `dropped` counter. **Why (the load-bearing reliability call):** `usage` (billing) and
`audit` (compliance) must **not** be dropped; diagnostic logs are explicitly droppable and can be
high-volume. Sharing one channel would let a chatty script's logs fill the buffer and silently drop
revenue/compliance events. Separate fates. *Alt rejected:* one shared channel with priority tiers —
more machinery, still couples the failure domains.

### D5 — Response mirror is gateway-gated, never caller-asserted; capture-on-failure
The `/execute` response carries `logs[]` **only** when nexus sets a **trusted** capture flag on the
request; it is drained on both the success and error paths so a log-then-throw run returns its
partial trail. **Why:** the playground needs inline logs, and capture-on-failure is exactly what you
want when a run dies mid-edit. Gating on a *trusted* flag (from the gateway, which is the only thing
that can reach the box) means an untrusted end-user can neither force capture (overhead/exfil) nor
read another tenant's logs. **Capture is orthogonal to `mode`** (OQ1): a *live* run may request
capture to debug real traffic; a *test/playground* run is response-mirror-only and does not stream.
*Alt rejected:* a caller-supplied `debug: true` in the request body — unsafe if the box were ever
exposed; the trusted-header layer is the right boundary.

### D6 — Level floor checked in the JS wrapper *before* building the entry
A `config.log_level` floor (default **`info`**); the `log.<level>` wrapper returns immediately if the
level is below the floor, **before** stringifying properties. **Why (Pino's trick):** logging is on
the always-on hot path of every request; a stripped `log.debug` must be nearly free (no allocation,
no FFI). The trusted gateway MAY **lower the floor per-request** for a capture run so the playground
gets `debug`/`trace` while production stays `info`+ (OQ2). *Alt rejected:* filter server-side after
capture — still pays the per-call JS + marshalling cost.

### D7 — Bounds mirror `emit`, sized to Vercel's per-request triad (OQ4)
Reuse the `max_ops`-style per-execution cap and add a per-entry size bound, with defaults anchored to
Vercel's per-request function-log limits: **256 entries/execution, 256 KB/entry, 1 MB total/execution**
(the total binds first; an oversize entry is truncated with a `truncated` marker, like Cloudflare's
`$cloudflare.truncated`). **Why:** one execution must not flood the channel or bloat a playground
response; these are the current industry per-request-log limits, and the discipline matches
`emit-effects-channel`. All three are configurable.

### D8 — Logs are outside the reproducibility contract
Ordering is a deterministic **`seq`** counter (safe under `Profile::Deterministic`). A relative
**microsecond offset** is included only on the non-deterministic `Profile::Full` path, since
`Profile::Deterministic` `delete`s the clock. Logs never influence `data`/`effects`. **Why:** a
deterministic script is often exactly the one you want to debug, so don't forbid logging under it —
just keep timing (and logs generally) out of the bit-reproducible outputs.

### D9 — Reuse the `emit-effects-channel` engine machinery; core stays domain-agnostic
A `log` injector + native `__log` FFI beside `inject_emit`; a bounded per-invocation buffer of
`(level, template, properties_json, seq)` drained into a public `LogEntry` type; threaded
`ExecResult → Outcome`. The core defines *structure* (levels, fields, order) but never interprets a
log's meaning; the stream/response routing lives entirely in `crates/runlet` (the edge), like the
identity/events layer. **No `runlet-wire` change** (edge-local).

## Risks / Trade-offs

- **Always-on capture cost on the request hot path** → D6 (level floor checked first) + D7 (bounds);
  the stream channel is non-blocking and drop-on-full, so a burst degrades logs, not latency.
- **Logs starving billing/audit** → D4 (separate bounded channel + separate dropped counter).
- **Info-leak / exfil via response logs** → D5 (trusted, gateway-asserted flag only; never
  caller-asserted; another tenant's logs are unreachable).
- **Playground response bloat** → per-execution cap + per-entry size bound; the mirror is attached
  only when the trusted flag is set.
- **Determinism leak** → D8 (logs excluded from `data`/`effects`; timing only on the `Full` path).
- **Two near-identical buffers (`emit` + `log`)** → accepted: the *policies* diverge on every axis
  (levels, gating, prod-stripping, determinism); a unified "sink" abstraction would relocate the
  divergence into conditionals. Revisit only if a third captured side-channel appears.

## Migration Plan

- Purely additive: a new `log` global (no existing script uses it), a new `logs` response field
  (absent unless the trusted flag is set → byte-compatible with today's response), and a new
  tenant-stream event kind on its own channel. No `runlet-wire` / `fabricd` protocol change.
- Rollback: edge-local + a core injector; reverting removes the global, the field, and the event
  kind with no state to unwind (logs were never durable).

## Resolved Decisions (locked, industry-anchored)

These were the design open questions; each is now locked to a current industry standard.

- **OQ1 (test-vs-live) — RESOLVED: Stripe's isolation model.** Every execution carries a
  gateway-asserted **mode** (live vs test/playground), orthogonal to the capture flag. A **live**
  run streams to the production tenant log stream (always-on) and, if capture is requested, *also*
  mirrors on the response (debugging a real prod call is allowed). A **test/playground** run is
  tagged test-mode and **never enters the live stream, billing, or audit** — v1 is
  **response-mirror-only** (test logs return inline, stream nowhere durable); a separate
  test-namespace stream is the growth path. *Anchor:* Stripe test/live are completely isolated,
  every object carries `livemode`, and the guidance is "include mode in all logging; never let test
  data reach live." *Note:* the full test-vs-live **mode** is a platform-wide dimension (it also
  gates billing and real side effects); this change only *consumes* the mode signal for log routing.
- **OQ2 (level floor) — RESOLVED: AWS Lambda's model.** A single `config.log_level` floor
  (default **`info`**, the Pino/industry production default), with the **trusted gateway able to
  lower the floor per-request** for a capture run (so production stays `info`+ and the playground
  gets `debug`/`trace`). Per-sink floors deferred. *Anchor:* Lambda `APPLICATION_LOG_LEVEL` +
  per-invocation debug elevation.
- **OQ3 (`log.with` collision) — RESOLVED: Pino/Serilog semantics.** The derived logger shares the
  buffer + `seq`; on a key collision **the per-call property overrides the bound-context value**
  (the most-specific/call-site value wins). *Anchor:* Pino child bindings / Serilog enrichers.
- **OQ4 (bounds) — RESOLVED: Vercel's per-request triad.** Defaults: **256 entries per execution**,
  **256 KB per entry**, **1 MB total per execution** (the total binds first; an oversize entry is
  truncated with a `truncated` marker). Level floor default **`info`**. All configurable. *Anchor:*
  Vercel (256 KB/line, 256 lines/request, 1 MB total) — the closest per-request-function analog;
  Cloudflare Workers truncates at 256 KB with a `$cloudflare.truncated` field; CloudWatch is 1 MB
  per event (2025).
- **OQ5 (flag transport) — RESOLVED: Stripe's auth-derived model.** Both the **mode** and the
  **capture** signals arrive as **trusted headers** (following the existing trusted-identity header
  convention), resolved in the identity layer — never caller body fields, defaulting to
  `(live, no-capture)`. *Anchor:* Stripe derives mode from the key/auth (not the request body);
  aligns with runlet's trusted-header design and N6 `traceparent` continuation.

## Build-vs-Adopt Gate

Ran `/opsx:decide` over the four critical concerns (security/reliability/correctness). Outcome:
**no new dependency** — every concern extends an already-adopted foundation or builds a trivial guard
on top of one. Research confirmed the one genuine adopt-temptation (a logging crate) does not fit: the
Message Templates model is a language-neutral *spec* with no Rust implementation, and `tracing`/`slog`
target *Rust-side, compile-time* logging, whereas our events originate dynamically from **JS calls in
QuickJS** and must be captured into a per-tenant buffer — bridging them through `tracing` +
`tracing-capture` is more machinery than capturing structured entries directly and fights tracing's
compile-time-fields model. We adopt the *design* (Message Templates semantics + tracing's levels),
not a crate.

### Decision: QuickJS log FFI boundary (security) — Extend the rquickjs FFI bridge

- **Status**: approved
- **Why**: Reuse the vetted string-in / JSON-string bridge (`Function::new` + a `JSON.stringify`
  wrapper) that `emit` and every capability already use — a solved, security-sensitive boundary; a
  bespoke path only adds audit surface.
- **Considered**: Build a bespoke log-marshalling path (rejected — duplicates the capability/`emit`
  FFI contract).
- **Isolation**: a native `__log` + its JS `log.*` wrapper in `engine.rs`, the same seam as
  `inject_emit`.

### Decision: Tenant log-stream delivery + backpressure (reliability) — Extend the events.rs pipeline (own channel)

- **Status**: approved
- **Why**: The existing per-tenant pipeline already does non-blocking, drop-on-full delivery via a
  `tokio` bounded `mpsc` (`try_send`); logs get their **own** bounded channel + drop counter so a
  chatty script can never starve the precious `usage`/`audit` events (the D4 reliability invariant).
- **Considered**: Adopt a channel crate (`async-channel`/`crossbeam`) — rejected, `tokio` `mpsc` is
  already the adopted foundation and does exactly this; Build a custom lossy ring buffer — rejected,
  reinvents bounded backpressure.
- **Isolation**: a second `EventBody::Log` channel in `events.rs`, parallel to the `usage`/`audit`
  channel, behind the same `EventSink` seam.

### Decision: Structured-log model + message-template rendering + levels (correctness/DX) — Build on the spec (no dependency)

- **Status**: approved
- **Why**: No mature Rust tool fits a JS-sourced, runtime-dynamic, per-tenant-captured sink (see the
  gate summary above). Adopt the **Message Templates spec** semantics + **tracing's level model** as
  *design*; render `{name}` substitution **in JS** (where the properties object already lives — no
  Rust-side reparse) and represent entries with `serde_json` (already a dependency). Levels are a
  trivial ordered enum.
- **Considered**: Adopt `tracing` + `tracing-capture` (rejected — bridging dynamic JS calls into
  tracing `Event`s fights its compile-time-fields model, more machinery than direct capture); Adopt
  `slog` (rejected — same Rust-side-logger mismatch).
- **Isolation**: the `log.*` JS wrapper renders + shapes entries; a `LogEntry` (`serde_json`-backed)
  crosses the FFI as the string-in/JSON-string payload; the level enum lives in `runlet-core`.

### Decision: Response-mirror capture gating (security) — Extend the trusted-identity layer

- **Status**: approved
- **Why**: Carry the diagnostic-capture flag through the existing trusted-header / identity / authz
  plumbing so it is **gateway-asserted, never caller-asserted** — the same trust boundary that
  already gates member capabilities and derives tenant identity; a separate debug-auth path would be
  a second, weaker gate.
- **Considered**: Build a standalone debug-auth mechanism (rejected — duplicates and weakens the
  trusted-header boundary).
- **Isolation**: the capture flag resolves in `identity.rs`/`authz.rs` alongside the other trusted
  signals; `handler.rs` reads the resolved flag, never a raw request field.
