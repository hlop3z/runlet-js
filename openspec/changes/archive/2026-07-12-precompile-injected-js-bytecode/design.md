## Context

Per `engine::run`, every request builds a fresh `Context::full` and re-runs the full injection
pipeline by `Context::eval`-ing framework JS source strings (`inject.rs`, `channels.rs`), then
materializes value-util wrappers from source on first access (`std_lazy.js` + the lazy builder).
Only the tenant's **handler** source is bytecode-cached today (`classify.rs:170-211`, the sole
audited `unsafe Module::load` at `:202`); the ~13.5 KB framework surface and ~55 KB of wrappers
are re-parsed. Profiling (this session, Docker/musl): compute-only baseline **1.075 ms**;
`one_util` **+1.53 ms**; bytecode `load` **~3× cheaper** than `compile` at multi-KB sizes.

Constraints: strict lint gauntlet (forbids `unsafe` except the audited self-produced-bytecode
load; no `unwrap`/`expect`/`panic`/bare-arithmetic/`as`); Docker-only build; rquickjs 0.12 with
the `futures` feature **off**; QuickJS synchronous/single-threaded; fresh-context-per-request is
a **security** invariant, not an accident (`pool.rs:4-5`).

## Goals / Non-Goals

**Goals:**
- Replace per-request parse+compile of the injected framework surface with a **load of
  precompiled bytecode**, produced **once per pool** and loaded into each **fresh** context.
- Same for the lazy value-util wrappers: first-access materialization loads bytecode, not source.
- Amortize `run_gc` off the every-release path.
- Ship a `runlet-bench` arm that measures source-injection vs bytecode-injection directly (the
  acceptance number).

**Non-Goals:**
- No context reuse / realm sharing (that is the out-of-scope Strategy C1; would need a
  prototype-pollution containment proof).
- No async egress (out-of-scope Strategy B; needs the `futures` feature + pool-slot spike).
- No change to the `{data, error, meta, effects, logs}` contract, injection *order*, capability
  semantics, determinism behavior, or any operator-facing config surface.
- No on-disk / cross-process bytecode persistence.

## Decisions

### Decision: Bytecode compile/serialize/load mechanism — Adopt rquickjs `Module::write`/`Module::load`

- **Status**: approved
- **Why**: The linked engine already ships a bytecode serializer we use for handler source (the sole audited `unsafe Module::load`); extending it to the injected surface adds no dependency and no second `unsafe` site. Produced at runtime in `JsPool::new` and loaded per fresh context (not the build-time `embed!` macro) so it keeps the existing "self-produced in-process" safety story and avoids build.rs/macro coupling; the one-time boot compile is negligible.
- **Considered**: build-time `rquickjs::embed!` (zero runtime compile, but couples to build config and diverges from the runtime-produced/runtime-loaded pattern); a hand-written serializer or JS-string cache (reinvents the engine, earns a second `unsafe` — rejected).
- **Isolation**: behind the existing `bytecode.rs` helper (extended) and the pool's precompiled-blob store; the audited SAFETY justification at `classify.rs:202` is reused verbatim.

### Decision: Injected-JS bundling — Build (hand-authored module, no JS toolchain)

- **Status**: approved
- **Why**: The injected surface is ~9 small first-party files already embedded via `include_str!`; hand-authoring the module wrapper avoids adding a JS bundler to a Rust-only musl/distroless pipeline, and the semantic work (implicit-global → explicit `globalThis` install) is manual regardless of any bundler. A justified Build — the "mature tool" buys nothing here and costs build-environment complexity + supply-chain surface.
- **Considered**: `esbuild-rs` in `build.rs` (real bundler, but pulls a Go-built DLL/MemoryModule into the static musl build — rejected on build-environment grounds); SWC (Rust-native but a transformer needing a bundler layer, large dep/cargo-vet surface — overkill).
- **Isolation**: the bundling lives entirely in first-party `crates/runlet-core/src/js/` sources + the `inject.rs` load path; no new build step, no new crate.

### D1. Reuse rquickjs `Module::write`/`Module::load` — Adopt/Extend, not Build

Extend the mechanism already in `bytecode.rs`/`classify.rs` from handler source to the injected
surface. No new dependency; reuses the existing, already-audited self-produced-bytecode `unsafe`
justification. *Alternative rejected:* a bespoke serialization or a JS-string cache — reinvents
what rquickjs already gives and earns a second `unsafe` site.

### D2. Bundle the injected surface as module(s), because rquickjs bytecode is module-only

The framework scripts are classic-script `eval`s today; rquickjs 0.12 exposes bytecode only via
`Module`. So each precompiled unit becomes a small **ES module** whose top-level code performs the
same global installation the classic script did — but modules are scoped, so any implicit global
(`function foo(){}` / `var x` at top level) must become an **explicit `globalThis.foo = …`**.
Native prerequisites (`__io`, `__ffi`, the decimal/sys/template natives) are registered *before*
the module loads, exactly as today, so the module's top-level code can reference them.
*Alternatives:* (a) one big bundled module vs (b) one module per script — pick per-script granularity
for the value-util wrappers (they load lazily and independently) and a single bundle for the
always-injected framework surface (one load per request beats N loads). Confirm during apply
whether rquickjs 0.12 has any classic-script compile entry point that would avoid the module
wrapping; plan assumes it does not.

### D3. Precompile once at pool construction; store `Arc<[u8]>` blobs on the pool

`JsPool::new` compiles each unit to bytecode once (via a throwaway context on a pool runtime) and
holds the blobs (`Arc`, shared across the pool). Per request, `Module::load` from a blob into the
fresh context. Self-produced in-process ⇒ load is sound; mirror the existing SAFETY comment
verbatim. *Alternative rejected:* compile lazily on first request — adds a cold-start cliff and a
synchronization point for no benefit; the surface is fixed and known at boot.

### D4. Keep profile-specific and per-request steps dynamic (post-load)

Precompile only the **profile-invariant, data-free** JS. The per-request/profile pieces stay as
they are: `__default_currency` set, `register_std_builder`'s profile-prune choice, the
`Deterministic` datetime/uuid pruning and `Math.random`/`Date` neutralization, the profile-gated
`io` mux + allowlist, and all native `Function::new` closures (they capture per-request buffers).
So there is **one** shared bytecode set; determinism is still applied as the existing post-load
prune. *Alternative rejected:* per-profile bytecode — doubles the store to save a tiny prune.

### D5. Amortize GC with a per-pool release counter

Replace `run_gc`-every-release (`pool.rs:132`) with run-every-N-releases (N a tuned constant,
revisited under load). *Alternative:* memory-pressure-triggered GC — more precise but pulls in
runtime memory introspection; defer unless the constant proves inadequate.

### D6. The bench arm is the acceptance gate

Add to `runlet-bench` a warm-host arm comparing source-injection vs bytecode-injection on
compute-only and one-util handlers. The cross-bench proxy this session was too noisy to isolate
the addressable fraction; this arm yields the real before/after and decides whether the
value-util-wrapper half earns its complexity (framework-only is the fallback if wrappers underperform).

## Risks / Trade-offs

- **Module scoping changes behavior** (implicit globals become module-scoped) → each converted
  script explicitly assigns to `globalThis`; a golden test asserts the injected surface is
  byte-for-byte equivalent to source-eval (same globals, same freeze state) before/after.
- **Injection-order / native-dependency breakage** (a module runs before a native it references)
  → preserve the exact existing order; natives register first, bytecode loads after; covered by
  existing engine tests + the new equivalence test.
- **Bytecode is rquickjs-version-specific** → produced in-process at boot, never persisted; no
  cross-version load path exists. Same guarantee as the existing handler-bytecode cache.
- **GC amortization raises steady-state memory** between collections → conservative N; watch RSS
  under the load test; fall back to per-release GC if regressive.
- **The win is smaller than hoped** (esp. wrappers) → D6's bench gates scope; ship framework-only
  if the wrapper half doesn't pay for its complexity.
- **New failure surface at boot** (a precompile error) → fail fast at `JsPool::new` with a clear
  error; a boot that can't compile its own fixed surface must not start (fail-closed).

## Migration Plan

Internal optimization, no API/config/wire change. Ship behind the normal build; no flag needed
(behavior is byte-identical). Rollback = revert the change; no data or format migration. If GC
amortization proves risky it can revert independently (it is orthogonal to the bytecode work).

## Apply-phase findings (recon before editing)

- **RESOLVED — bytecode is module-only.** rquickjs 0.12's only serializer is `Module::write`
  and its only loader `Module::load`; the write path compiles with
  `JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY` (`rquickjs-core-0.12.0/src/value/module.rs:267`).
  There is **no** classic-script compile/serialize in the safe API, so D2's module-wrapping is
  mandatory, not optional.
- **Framework-surface conversion is low-risk.** Every injected framework script
  (`std.js`, `bridge.js`, `ffi.js`, `std_lazy.js`, `std_project.js`, `std_freeze.js`, `io.js`,
  `log.js`) is already IIFE-wrapped and installs its effects via explicit `$std.`/`globalThis.`
  assignment; bare `function`/`var` are function-scoped inside the IIFE. Audit found **no
  top-level `this`-as-global reliance and no `with()`**, so the forced strict mode of a module is
  behavior-neutral, and QuickJS module free-variable reads still resolve to the globals prior
  scripts set. Each script becomes a one-line module that runs its existing IIFE at its existing
  injection point (they are NOT one contiguous bundle — they are interleaved across phases with
  native registrations and the user script, so per-script modules, not a single bundle).
- **NEW WRINKLE — value-util wrappers are assembled per-request, not static.** The wrappers are
  not eval'd as fixed source: `register_std_builder`/`shadow_unit` (`engine/inject.rs:129-206`)
  build a **per-request** shadow-eval string per unit that bakes in the deterministic prunes
  (`delete $std.datetime.now` / `$std.crypto.uuid`) and the sys context post-step, and wraps the
  wrapper body in a fresh-scratch-realm scaffold. A per-request-varying string cannot be
  precompiled directly. Group 4 therefore needs a refactor that **separates the static wrapper
  body (precompilable to a per-unit module) from the dynamic scaffold + prunes (applied at
  materialization time)** — larger and more delicate than "precompile the wrappers" implied.

## Measured result

Same-host A/B (Docker/musl), `mux` bench, control = `RUNLET_DISABLE_SURFACE=1` (source-parse),
variant = default (bytecode). The control is byte-identical to the variant (it *is* the fallback
path), so any delta is pure bytecode-vs-parse — no host-load drift.

**Group 3 only (framework surface):**

| Arm | Control (source) | Variant (bytecode) | Speedup |
|---|---|---|---|
| `baseline_0_calls` (compute-only) | 898.9 µs | 610.3 µs | 1.47× |
| `one_util` | 2.325 ms | 1.944 ms | 1.20× |
| `all_utils` | 5.018 ms | 4.917 ms | 1.02× |

The flat `all_utils` (1.02×) confirmed its ~5 ms was per-request wrapper-build cost that only
Group 4 addresses — the D6 evidence that the wrapper half was worth doing, not dropping.

**Groups 3 + 4 (framework surface + value-util wrappers):**

| Arm | Control (source) | Variant (bytecode) | Speedup |
|---|---|---|---|
| `baseline_0_calls` (compute-only) | 862.8 µs | 568.4 µs | **1.52×** |
| `one_util` | 2.214 ms | 1.009 ms | **2.19×** |
| `all_utils` | 5.095 ms | 2.064 ms | **2.47×** |

Reading: compute-only ~2.4k → ~3.6k RPS; a handler touching one value-util **2.2× faster**; a
heavy util handler **2.5× faster**. Real business handlers use money/datetime, so the realistic
gain is ~2×. **No regression** on any arm. (An earlier cross-run compare suggested Group 3 was
−43%; the rigorous same-host toggle corrects that to −32% — the difference was host-load drift.)

## Open Questions

- Final GC cadence N (D5) — set by the load test, not guessed.
- Group 4 shape: does the static/dynamic split above clear a worthwhile speedup bar (D6), or do
  we ship framework-surface-only (Group 3) and defer the wrappers?
