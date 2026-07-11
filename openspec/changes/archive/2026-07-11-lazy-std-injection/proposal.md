## Why

Every `/execute` request builds the **entire** `$std` value-util namespace from source into its
fresh QuickJS context — ~14 eval passes / ~74 KB of JS (`decimal`, `money`, `sys`, `datetime`,
`text`, `list`/`dict`, `template`, `check`) — whether or not the handler touches any of them.
Measured on a release build (thin-LTO, 16-core Docker): a full trivial invocation costs **4.77 ms**
(`mux/baseline_0_calls`) and is I/O-independent (ten `io.call`s add only 90 µs), so nearly all of it
is fixed sandbox bootstrap. A spike that skipped the value-util injections dropped invocation to
**0.76 ms** — the value-util build is **~84 % of per-request cost**. This caps single-node
throughput at **~2,400 RPS** (k6), and every new `$std.*` util taxes *every* request, used or not.

## What Changes

- **Materialize each `$std` member lazily, on first access within a request**, instead of eagerly
  building all of them up front. A handler that uses no value-utils pays only the structural
  bootstrap (~0.8 ms, ~6× faster); a handler pays only for the members it actually touches; a
  handler touching everything degenerates to today's cost. The win is usage-weighted.
- **The projected bare globals become lazy too.** `$` (mapped to `$std.money`) must build money on
  first *use*, not when the projection assigns it — otherwise projection eagerly constructs the most
  expensive util (money, 9 KB) on every request and defeats the change. `json`/`log`/`emit` are
  cheap and per-request, so they stay eager.
- **The lazy builder is determinism-aware.** Under `Profile::Deterministic` there is no built object
  to prune until a member is touched, so the builder constructs the already-pruned variant on first
  access (replacing today's build-then-delete step).
- **No observable behavior changes.** `$std.<member>` and its projected global remain defined,
  identity-equal (`$ === $std.money`), deep-frozen before `handler` runs, and profile/config-gated
  exactly as today. Lazy materialization is transparent to the script.
- **Isolation is preserved by construction.** This keeps the fresh-`Context`-per-request model
  unchanged — lazy building only changes *when* a member is constructed *within* a request, never
  that each request gets a pristine realm. No shared/warm realm, no cross-request state, no new
  isolation surface.

## Capabilities

### New Capabilities

_None._ This is a behavioral refinement of an existing capability, not a new one.

### Modified Capabilities

- `std-namespace`: members are materialized **lazily on first access** rather than eagerly per
  request. Adds the requirement that lazy materialization is **observationally equivalent** to
  eager injection — identity-equality of projected globals, deep-freeze, the determinism prune, and
  profile/config gating all still hold — and that a member is built **at most once per request** so
  the projected global and the namespace member remain the same reference.

## Impact

- **Code:** `crates/runlet-core/src/engine.rs` (the per-request injection sequence in `run`,
  `project_std_globals`, `freeze_std`, and the determinism seam in `harden`), the per-util
  `inject_*` entry points (`decimal`/`money`/`sys`/`datetime`/`text`/`collections`/`template`/
  `check`), and the JS bootstrap (`js/std.js`, `js/std_project.js`, `js/std_freeze.js`,
  `js/determinism.js`).
- **Behavior/API:** none. The `$std` surface, the four bare globals, and the response envelope are
  unchanged; the `container/types.d.ts` / `base.d.ts` `.d.ts` surface is unchanged (the D11 golden
  test `types_dts_is_up_to_date` must still pass).
- **Invariants preserved (not modified):** `execution`'s "per-request isolation and sandbox limits"
  requirement — each request still runs in a fresh context with no cross-request global leakage.
  This change explicitly does **not** touch that guarantee.
- **Performance (the point):** util-free handlers ~4.77 ms → ~0.8 ms; expected single-node ceiling
  rises from ~2,400 RPS toward the point where the HTTP/tokio stack, not the engine, becomes the
  bottleneck.
- **Rejected alternatives** (recorded in `design.md` so they are not re-litigated): shared/warm
  frozen realm reused across requests, a two-mode (bulletproof + fast) split, and bytecode-
  precompiling the bootstrap.
