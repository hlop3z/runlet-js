## Context

Today `engine.rs::run` builds a fresh QuickJS `Context` per request and eagerly injects the whole
`$std` surface into it — ~14 sequential `eval` passes of ~74 KB of JS wrapper source (`std.js`,
`bridge.js`, then `decimal`/`money`/`sys`/`datetime`/`text`/`collections`/`template`/`check`,
`emit`/`log`, `project`, `freeze`) — regardless of what the handler uses. Measurements this session
(release, thin-LTO, 16-core Docker):

- `mux/baseline_0_calls` (a full trivial invocation) = **4.77 ms**, I/O-independent (ten `io.call`s
  add 90 µs). So the cost is fixed bootstrap, not the handler or egress.
- A spike that skipped the value-util injections = **0.76 ms** → the value-util build is **~84 %** of
  per-request cost.
- End-to-end k6 single-node ceiling = **~2,400 RPS**, bound by this per-request work (pool sized to
  cores; raising `pool_size` past cores made it worse).

The per-request cost is dominated by *parsing and executing the JS wrapper IIFEs*, not by
registering the small native FFI bridges (`__decimal`, `__sys`, `__template`) — those are cheap
QuickJS closures. And the sandbox's core guarantee is **per-request isolation**, currently free by
construction (a fresh realm per request).

## Goals / Non-Goals

**Goals:**
- Stop paying to build `$std` members a handler never touches; make the per-request bootstrap
  usage-weighted (util-free handlers ≈ 0.8 ms, ~6×).
- Preserve every observable behavior of `std-namespace`: reachability, identity-equal projected
  globals, deep-freeze immutability, determinism prune, profile/config gating, and the `.d.ts`
  surface (D11 golden test).
- Preserve `execution`'s per-request isolation **by construction** — no change to the
  fresh-`Context`-per-request model.

**Non-Goals:**
- Any form of shared/warm realm reuse across requests (see Decisions → rejected alternatives).
- A second "fast, less-isolated" execution mode.
- Changing the authored `$std` API, the four bare globals, or the response envelope.
- Optimizing the residual structural bootstrap (`std`/`bridge`/`emit`/`log`/`project`/`freeze`,
  ~0.76 ms) — out of scope; this change targets only the value-util build.

## Decisions

### D1 — Lazy materialization via non-configurable getter-only accessors on `$std`

Each value-util member is installed on `$std` as a **non-configurable, getter-only accessor**
(`Object.defineProperty($std, name, { get, enumerable: true, configurable: false })`). On first
access the getter builds the member, deep-freezes it, memoizes it in a closure variable, and returns
it; subsequent accesses return the cached frozen instance. Untouched members are never built.

- **Why getter-only + non-configurable:** it is the *lock*. No setter ⇒ `$std.money = evil` silently
  fails / throws; `configurable: false` ⇒ the slot cannot be `delete`d or `defineProperty`-redefined
  by the handler. This satisfies "the member set is locked before `handler` runs" *without* a
  self-replacing data property (which would need `configurable: true` and thus be deletable). The
  small per-access getter overhead is negligible next to a 74 KB eval.
- **Build-vs-adopt:** Build (extend the engine). The mechanism is standard ECMAScript accessor
  semantics plus the existing JS `Object.freeze`; there is no external library to adopt. (SES /
  hardened-JS "lockdown" was evaluated only for the rejected warm-realm path below.)
- **Alternatives:** eager (today, the cost we're removing); self-replacing configurable getter
  (rejected — deletable slot weakens the lock); a `Proxy` trap (rejected — `Proxy` is *removed* by
  the sandbox hardening, and would add per-access overhead to every property).

### D2 — Cheap native FFI bridges stay eager; only the expensive JS wrappers are lazy

The costly part is eval'ing the wrapper IIFEs; registering the native bridges (`__decimal`, `__sys`,
`__template`) is microseconds. Keep the natives (and per-request scalars like `__default_currency`
and the `__sys` secrets closure) **eager**, and make only the **JS wrapper construction** lazy.

- **Why:** it keeps the inter-member dependency graph trivial. `money` composes over `__decimal`,
  `list`/`dict` aggregates use `__decimal`, `datetime` rides `__sys` — with all natives present up
  front, a member's lazy builder never has to force-build another member's *native*. If a wrapper
  legitimately needs *another wrapper* (rare), it accesses it via `$std.<dep>`, which lazily builds
  that dep on demand — the dependency resolves through the same accessor path.
- **Validation (task 1.2, done):** measured via a throwaway `SPIKE_NATIVES_ONLY` gate that registers
  every native bridge (`__decimal`/`__sys`/`__template`/`__default_currency`/secrets) but skips all
  wrapper IIFE evals (and the `$std` projection + freeze, which have nothing to act on). On this
  branch (thin-LTO release, 16-core Docker) the full baseline re-measured at **~5.02 ms** (task 1.1;
  range [4.91, 5.14]), and natives-only dropped to **~572 µs** (range [561, 585]) — i.e. the entire
  structural bootstrap *plus* every native registration is ~11 % of cost, and the wrapper
  parse+execute is the other ~89 %. Native registration alone is a rounding-error slice of that
  ~572 µs floor. **D2 holds: keep natives eager, make only the wrappers lazy.** A util-free handler
  should land near this ~0.57–0.8 ms floor once §2 lands (~8–9× vs 5.02 ms).
- **Alternative:** fully lazy including natives (rejected — forces per-member dependency ordering and
  buys almost nothing, since natives are cheap).

### D3 — Projected bare globals are lazy accessors that funnel to the `$std` member

`$` (→ `$std.money`) is installed on `globalThis` as a getter that returns `$std.money` (triggering
that member's lazy build). It is **not** an eager `globalThis.$ = $std.money` assignment.

- **Why:** an eager projection *reads* `$std.money`, firing its getter and building the single most
  expensive util (money, 9 KB) on **every** request — silently defeating the entire change. Routing
  the global through the member's accessor also **guarantees identity** (`$ === $std.money`): both
  paths return the one memoized instance.
- `json`/`log`/`emit` carry per-request state (buffers) and are cheap; they stay eager as today.
- **Locking:** the exposed global bindings are locked non-writable before `handler` runs regardless
  of whether the backing member has materialized (a getter-only, non-configurable global binding).

### D4 — The lazy builder produces the determinism-pruned variant directly

Under `Profile::Deterministic`, the builder constructs the already-pruned member on first access
(no `datetime.now` / `crypto.uuid` / etc.), instead of today's "build full, then delete" step in
`js/determinism.js`. Freeze then applies to the pruned object.

- **Why:** with lazy build there is no eagerly-built object sitting around to post-prune before the
  handler runs; folding the prune into the builder keeps the "prune-before-freeze" ordering intact
  *per member* and guarantees no un-pruned alias is ever materialized or frozen.
- **Mechanism:** the builder is parameterized by profile; the determinism sanitizer logic moves from
  a post-hoc global pass into the per-member build (or the member's wrapper reads a profile flag).
- **Alternative:** keep the post-hoc `determinism.js` pass, forcing all prunable members to build
  eagerly so there is something to prune (rejected — reintroduces eager cost for exactly the members
  a deterministic compute handler is most likely to skip).

### D5 — Keep fresh-`Context`-per-request isolation; reject all shared-realm approaches

Isolation stays a property of the architecture, not of a freeze/prune audit. Rejected alternatives,
with the evidence that killed them:

- **Warm / shared frozen realm reused across requests.** A spike proved **4.77 ms → 35.7 µs (134×)**,
  output verified correct. Rejected because reusing the realm (a) lets primordial/prototype pollution
  from request A survive into request B unless we add full SES-style "lockdown" (freeze every
  primordial) and prove the per-request global-prune is complete — trading isolation-by-construction
  for isolation-by-audit, the wrong trade for a sandbox whose whole value is isolation; and (b) the
  util wrappers read per-request state via bare globals (`money` reads `globalThis.__default_currency`
  and calls `__decimal`/`__sys` as free variables), which bind their **defining** realm — so sharing
  the actual objects (`Persistent`) across contexts cross-binds one request's currency/secrets into
  another. If lazy injection reaches ~0.8 ms, the ~130× ceiling is unnecessary.
- **Two modes (bulletproof default + fast single-trust-domain warm realm).** Considered and dropped:
  ~0.8 ms already puts the ceiling near where the HTTP/tokio stack — not the engine — bottlenecks, so
  the fast mode buys a ceiling we won't hit while adding a second security-critical per-request code
  path and a misconfiguration footgun.
- **Bytecode-precompiling the bootstrap.** Saves only ~20–30 % (the `bytecode` bench: `compile`→`load`
  saves *parse* time, not object construction), far less than skipping unused builds entirely, and is
  subsumed by lazy injection.

### Decision: Lazy `$std` materialization + per-member immutability — Build/Extend (hand-written on rquickjs)

- **Status**: approved
- **Why**: No library materializes *our own* bespoke `$std` namespace lazily; the mechanism is inherent ECMAScript accessor semantics + `Object.freeze` over rquickjs, which is already adopted.
- **Considered**: Adopt **SES / Hardened JavaScript** (Endo `ses`, lockdown + Compartments) — hard-rejected: it is the *shared-realm + lockdown* model (the warm-realm architecture rejected in D5), it runs by eval'ing a large shim into the realm (inflating the exact per-request bootstrap cost this change removes), and it cannot build our custom members. Its endowment model (`Math.random`/`Date.now`/`new Date()` disabled and injected on demand) *validates* our determinism approach but is not a dependency we can take on QuickJS-in-Rust.
- **Isolation**: lives entirely behind the engine's per-request injection path (`engine.rs::run` + the per-util `inject_*` split of §D1/D2); the `$std` object graph is the boundary.

### Decision: Determinism prune under lazy build — Extend (our own determinism surface)

- **Status**: approved
- **Why**: The prune list (`datetime.now`, `crypto.uuid`, `Math.random`, no-arg `Date()`) is our own; folding it into the per-member builder (D4) reuses existing `js/determinism.js` logic. SES confirms "disable + inject on demand" is the sound pattern.
- **Considered**: Adopt SES endowments — hard-rejected for the same reasons as above (can't take the library into QuickJS-in-Rust without the shim cost).
- **Isolation**: lives behind the per-member builder + a small residual global sanitizer for JS builtins that are not `$std` members (`Math.random`/`Date()`), per the Open Question.

## Risks / Trade-offs

- **Accessor overhead / identity regression** → getters are ~free vs a 74 KB eval; identity is
  guaranteed by funneling both the member and its global through one memoized cache (D3). Covered by
  spec scenarios (identity, at-most-once) and unit tests.
- **A member escapes freezing because it builds *during* the handler** → the getter deep-freezes the
  member *before* returning the reference, and the container slots are non-configurable getter-only
  from setup (D1), so a handler never obtains a mutable member or an unlocked slot. This is the
  explicit MODIFIED freeze requirement.
- **`Object.keys($std)` / enumeration forcing builds** → enumeration does not fire getters, so
  listing keys does not build members; only *reads* (including destructuring `const { x } = $std`,
  which is a read) build. Acceptable and specified.
- **Determinism variant drift** → the builder must produce exactly the surface today's post-hoc
  prune leaves. Mitigation: reuse the same pruned-member definitions; assert via the existing
  determinism scenarios plus the new deterministic-lazy scenarios.
- **"Natives eager" reintroducing fixed cost** → validate native registration is a small fraction of
  4.77 ms (D2 task); if not, move the heaviest native registration behind its wrapper's getter too.
- **Hidden Rust-side state in a native (e.g. `template`/minijinja caches)** → out of scope for
  isolation here (shared identically under eager and lazy), but note it: laziness changes *when* a
  native is first called, not whether it holds cross-request state. No change to that surface.

## Migration Plan

- Pure internal refactor of the engine's per-request injection; **no API, wire, config, or `.d.ts`
  change**. No data migration, no client action.
- Rollout is a straight binary replacement; rollback is redeploying the prior image.
- Correctness gate before merge: full `cargo test` (unit + determinism scenarios), the Python
  integration suite, the D11 golden test, and a re-run of `mux/baseline_0_calls` (expect util-free
  ≈ 0.8 ms) plus a k6 single-node RPS check to confirm the ceiling moved. Build/test are Docker-only
  (WDAC blocks native cargo); `cargo build` enforces rustc lints but **not** clippy — run
  `task clippy` separately.

## Outcome (apply)

- **Mechanism.** Getter-only, non-configurable accessors on `$std` (`js/std_lazy.js`); native
  `__stdBuild(key)` parses+executes each wrapper into a fresh scratch realm (`Object.create($std)` +
  own writable slots via `__stdMake`) so dependency reads fire other lazy getters and the wrapper's
  `$std.<name> = X` self-write is captured; the getter deep-freezes + memoizes. A probe
  (`engine::tests::lazy_std_accessor_mechanism`) validated the load-bearing assumption first: a
  native getter can `qctx.eval` a member's wrapper **nested inside the running handler**, after the
  JS `eval` global is removed. `sys` is one 3-member build-unit (crypto/env/secrets).
- **Determinism split (Open Question — resolved).** Confirmed as anticipated: the JS builtins
  (`Math.random`, no-arg `Date()`) stay in the post-eval `js/determinism.js` pass; the prunable
  `$std` members (`$std.datetime.now`, `$std.crypto.uuid`) moved into the lazy builder
  (`build_unit_sources` appends the `delete`s under `Profile::Deterministic`, before the return +
  freeze). The post-hoc pass no longer reads `$std.datetime`/`$std.crypto`, so it never force-builds
  an untouched member.
- **Performance (thin-LTO release, 16-core Docker).** `mux/baseline_0_calls` (util-free) **5.02 ms →
  1.10 ms (~4.6×)**; one util (money+decimal) **2.81 ms (~1.8×)**; all 11 members **6.85 ms** — the
  bounded worst case, ~36 % over the old eager build (the inherent per-build machinery, paid only
  when everything is touched). The win is usage-weighted exactly as intended; the ~1.1 ms util-free
  floor is above the ~0.8 ms spike estimate because the lazy install (accessors + projection getter +
  container freeze) is real per-request work the natives-only spike skipped.

## Open Questions

- Is any residual structural-bootstrap cost worth a follow-up (the ~0.76 ms floor: `std`/`bridge`/
  `emit`/`log`/`project`/`freeze` + the lazy-accessor install)? Out of scope here; revisit only if the
  post-change ceiling is still engine-bound rather than HTTP/tokio-bound.
- The all-utils worst case is ~36 % over the old eager cost. If a real workload is dominated by
  touch-everything handlers, the per-build machinery (the shadow's `Object.create` + nested frames +
  `__stdBuilt` round-trip) is the place to trim; deferred as premature for the target workload
  (business scripts touch a handful of utils).
