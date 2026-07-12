## 1. Measurement scaffold (before-number first)

- [x] 1.1 Baseline captured via the existing `mux` arms (`baseline_0_calls`, `one_util`, `all_utils`) driving a warm `LogicHost` — no new bench file needed. A same-host A/B was enabled instead by the `RUNLET_DISABLE_SURFACE` pool toggle (control = source-parse, variant = bytecode), which is more rigorous than a cross-run compare.
- [x] 1.2 Numbers recorded (scratchpad `bench_before.txt`/`bench_after.txt` + the A/B in design "Apply-phase findings").

## 2. Bytecode plumbing (extend beyond handler source)

- [x] 2.1 `classify::eval_surface_bytecode` (load + eval + `promise.finish`, self-produced-bytecode `unsafe` mirroring `load_bytecode`) + `inject::compile_unit` (`Module::declare` → `write`).
- [x] 2.2 `PrecompiledSurface` stored on `JsPool` (`Option<Arc<…>>`), compiled once in `JsPool::new` on a throwaway runtime; a compile failure fails pool construction (fail-closed).
- [x] 2.3 Resolved: bytecode is module-only (`module.rs:267`); module-wrapping is mandatory. Framework scripts are IIFE-wrapped with no top-level `this`/`with`, so strict-mode module conversion is behavior-neutral. See design "Apply-phase findings".

## 3. Framework injection surface → bytecode (every-request path)

- [x] 3.1 Converted the six `inject.rs` scripts (`std.js`, `bridge.js`, `ffi.js`, `std_lazy.js`, `std_project.js`, `std_freeze.js`) — loaded as per-script modules (source used verbatim; the IIFE wrapping means no implicit-global rewrite was needed). Source-parse fallback retained when no surface (`inject_unit`).
- [x] 3.2 Injection pipeline (`inject.rs` + `engine/mod.rs`) loads the precompiled bytecode in the exact existing order (natives register first, bytecode loads after); threaded via `ExecParams.surface` from `JsPool`.
- [x] 3.3 Equivalence golden test `engine/surface_bytecode_tests.rs`: byte-identical output source-vs-bytecode across a probe battery under both profiles, the determinism prune, and the cross-request no-leak scenario. All green.
- [ ] 3.4 (follow-up slice) Convert the `channels.rs` surface too — `log.js` (3 KB) and `io.js` (0.8 KB); the tiny inline emit-wrapper stays source (below the bytecode crossover). Deferred: the A/B shows the big win is already captured by 3.1 (the ~5 ms `all_utils` residual is wrapper-build cost, not these).

## 4. Value-util wrappers → bytecode (first-access path)

- [x] 4.1 Static-body/dynamic-scaffold split done: `shared_build_units()` (7 profile/request-independent units — decimal, money, text, list, dict, template, check) + `datetime`'s two profile variants precompile to bytecode in `compile_surface`. `sys` stays source-parsed (per-request env/secrets + det-crypto prune — the one genuinely dynamic unit).
- [x] 4.2 `register_std_builder` now takes the surface, builds the per-profile blob list (`build_unit_blobs`), and the `__stdBuild` native prefers a precompiled blob (`eval_surface_bytecode`), falling back to source-parse for `sys` and for the no-surface path. Deterministic prune is the correct precompiled `datetime` variant; sys-post stays in the source path.
- [x] 4.3 Equivalence test extended: probes materialize every wrapper (reachability probe fires money/decimal/text/datetime/list/dict/sys getters; added template/check/decimal-op probes) — byte-identical bytecode-vs-source under both profiles. All green.

## 5. GC amortization (orthogonal)

- [x] 5.1 `run_gc`-every-release replaced with a per-pool release counter running GC every `GC_EVERY_N` (=16) releases; independent of the bytecode work.
- [x] 5.2 Test `gc_is_amortized_not_per_release` (3 sweeps over 3·N releases); N recorded for load-test validation.

## 6. Acceptance measurement & scope decision

- [x] 6.1 Same-host A/B (`RUNLET_DISABLE_SURFACE` control vs default), Docker/musl. Groups 3+4: **baseline_0_calls 862.8 µs → 568.4 µs (1.52×)**, **one_util 2.214 ms → 1.009 ms (2.19×)**, **all_utils 5.095 ms → 2.064 ms (2.47×)**. No regression on any arm; control is byte-identical (fallback). Full table in design.
- [x] 6.2 D6 gate resolved: group 4 done (the −2% all_utils under group-3-only was the wrapper-build cost group 4 unlocked → 2.47×). Both halves shipped.

## 7. Verify & land

- [x] 7.1 Green in Docker for the slice: `cargo test --workspace` (96 + 130 + 12), `cargo test -p runlet-core --no-default-features` (178, incl. the new equivalence/isolation/GC tests), `cargo clippy --workspace` + `-p runlet-core --no-default-features`, `cargo fmt --all --check` — all clean. Response contract unchanged (equivalence test asserts byte-identical output).
- [x] 7.2 No dependency-graph change (no `Cargo.toml`/`Cargo.lock` diff — only `std::env` added), so `cargo vet` is a no-op vs the last green run; nothing to re-audit.
- [x] 7.3 Added a "Bytecode-precompiled injected surface" subsection to `docs/design/composable-core.md` (mechanism, module-only caveat, isolation guarantee, the A/B numbers, GC amortization).
