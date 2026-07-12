## Why

Per-request cost is dominated by re-parsing JavaScript that never changes. Profiling this
session (real `LogicHost`, Docker/musl) put the compute-only baseline at **1.075 ms/request**
(÷cores ≈ the ~2.4k RPS ceiling), and showed that **touching a single value-util costs
+1.53 ms** (`one_util` 2.61 ms vs baseline; `all_utils` 6.64 ms) — because the ~13.5 KB of
framework injection scripts are re-parsed on *every* request and the ~55 KB of value-util
wrappers are re-parsed on first access, none of it bytecode-cached (today's cache covers only
user ES-module source). A bytecode sweep measured **loading precompiled bytecode ~3× cheaper
than parse+compile** for multi-KB sources. This is the largest remaining per-request lever, it
helps every real business handler (which use `$`/money/datetime), and it can be captured
without weakening any isolation invariant.

## What Changes

- Precompile the **framework injection surface** (`std.js`, `bridge.js`, `ffi.js`,
  `std_lazy.js`, `std_project.js`, `std_freeze.js`, `log.js`, `io.js`, the emit wrapper) to
  QuickJS bytecode **once per pool**, then load it into each **still-fresh** `Context` per
  request instead of re-parsing from source.
- Precompile the **lazy value-util wrappers** (`money`, `datetime`, `list`, `dict`, `text`,
  `template`, `check`, `decimal`) to bytecode once, so first-access materialization loads
  bytecode rather than parsing ~55 KB of source.
- **Amortize GC**: stop running `run_gc` on every pool release; run it every-N releases / on
  idle instead (orthogonal, cheap).
- Add a **before/after benchmark arm** to `runlet-bench` (injection-from-source vs
  injection-from-bytecode) that measures the exact addressable speedup as an acceptance
  deliverable.
- **Not breaking**: the `{data, error, meta, effects, logs}` response contract is byte-identical;
  the fresh-context-per-request guarantee is unchanged (bytecode reuses compiled *code*, never
  *state*).

## Capabilities

### New Capabilities
<!-- None. No new user-facing behavior; this is an internal performance change. -->

### Modified Capabilities
- `execution`: the *Per-request isolation and sandbox limits* requirement is extended so that
  the isolation guarantee explicitly covers the **injected framework/value-util surface** — any
  precompiled or cached injected JS SHALL be reused as compiled code only, never as retained
  state, and each execution SHALL still run in a fresh context with no cross-request/tenant
  leakage. (Behavioral contract unchanged; the clause makes the optimization's boundary explicit.)

## Impact

- **Code**: `crates/runlet-core/src/engine/` (`inject.rs`, `classify.rs`, the injection
  pipeline in `mod.rs`), `crates/runlet-core/src/bytecode.rs` (extend beyond user modules),
  `crates/runlet-core/src/pool.rs` (warm-time precompile store + GC amortization),
  `crates/runlet-core/src/js/` (framework scripts may need bundling as a module — see design).
- **Mechanism risk (resolved in design)**: framework scripts are currently classic-script
  `eval`; rquickjs bytecode is module-only (the sole audited `unsafe Module::load` at
  `classify.rs:202`). Bundling the injected surface as a module is the likely path.
- **Dependencies**: none new (reuses the existing rquickjs `Module::write`/`Module::load`).
- **Tests/CI**: new `runlet-bench` arm; existing unit/integration suites must stay green
  (Docker-verified per project convention).
- **Out of scope (separate follow-on changes)**: async egress (rquickjs `futures`/`AsyncContext`,
  gated on a pool-slot spike + A/B benchmark) and full context-reuse (needs an intrinsic/prototype
  pollution-containment proof).
