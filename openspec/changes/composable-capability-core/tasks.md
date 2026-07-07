# Tasks: composable-capability-core

## 1. Registry + mux (behavior-preserving)

- [ ] 1.1 Add `CapabilityDef` (name, js_wrapper, trust: `OperatorSupplied | ScriptControlled(SsrfPolicy)`, backend: `Arc<dyn Egress>`) to runlet-core; reject duplicate names at build
- [ ] 1.2 Add `LogicHost::builder()` (capabilities + optional `fallback_egress`); keep the old constructor delegating to it temporarily. D1: `.build()` returns a non-generic `LogicHost` (capabilities held in `Vec`/`HashMap`, not a type parameter per capability) — add a compile-time check (e.g. a function returning `LogicHost` with no capability generics) so a future refactor can't reintroduce capability-typed generics into the published surface
- [ ] 1.3 Replace the single egress slot with the per-name mux (`HashMap<name, Arc<dyn Egress>>` + fallback) in `ExecParams`/`engine.rs::io_hook`; preserve `EGRESS_UNAVAILABLE` semantics
- [ ] 1.4 Route `inject_apis` from the registered def list instead of cfg blocks (standard defs registered internally for now — engine tests must pass unchanged)
- [ ] 1.5 Enforce D4: apply the SSRF guard inside the mux for `ScriptControlled` defs; unit test a script-controlled def hitting a private IP is rejected pre-connect
- [ ] 1.6 Enforce D9 fail-closed: an internal enforcement error/panic (metering, deadline-clock, trust-policy eval) denies the call; unit test that a mux with a failing metering/policy hook rejects rather than executes the I/O
- [ ] 1.7 D9 bypass surface: write down the enumerated unmediated-authority list (in-engine `http`/`s3`, clock, RNG, exit) in `docs/design/composable-core.md`; test that the deterministic profile *removes* ambient time/RNG/`$sys` (script cannot re-reach them), not merely stubs them
- [ ] 1.8 Unit tests: mixed topology (one name local `EchoEgress`, one via fallback), unregistered name → global undefined, deterministic profile injects nothing

## 2. Metrics + config dynamization

- [ ] 2.1 Replace cfg'd `ExecMetrics`/`Collectors` per-cap fields with a name-keyed collector map; drain into `Outcome`
- [ ] 2.2 Handler: replace fixed `Meta.<cap>_requests` with `meta.io.<name>` map; make `RequestConfig.io` generic over registered names
- [ ] 2.3 Verify `events.rs` usage-event dims against the new metrics source (no tenant-metering spec change)
- [ ] 2.4 Update `tests/test_simple.py` meta assertions and `container/types.d.ts` meta typings to `meta.io`

## 3. Preset extraction + core slimming

- [ ] 3.0 D10: define the shared action-token identities as a `const`/enum within this repo (`runlet-caps` or a shared module it and the core dispatch import) so a renamed verb is a compile error between wrapper and dispatch; the cross-repo seam to fabricd stays the `runlet-wire` string protocol + the fixture test (do not touch `runlet-wire` this change)
- [ ] 3.1 Create `runlet-caps` crate: the six standard `CapabilityDef`s (JS wrappers moved from core, trust declarations, action tokens sourced from the D10 shared enum, action-token fixture test vs fabric-backends names)
- [ ] 3.2 Flip the `runlet` bin to compose the preset + sidecar fallback (stock behavior parity)
- [ ] 3.3 Delete core's six wrapper modules + `js/*.js` for driver caps; remove the six cargo features; keep `http`/`s3`
- [ ] 3.4 Update `scripts/sweep.sh` to the `http`/`s3`-only matrix; confirm `--no-default-features` links no network dep (`cargo tree` check)

## 4. Verification + docs

- [ ] 4.1 Full gate: `task check` (fmt, clippy gauntlet, unit tests) + feature sweep
- [ ] 4.2 Integration suite vs sibling fabricd, plus a mixed-topology smoke (one capability bound in-process via `fabric-backends`, rest via sidecar fallback)
- [ ] 4.3 runlet-bench: confirm no measurable hot-path regression from the mux lookup
- [ ] 4.4 Update README (capability pattern + extension how-to), CLAUDE.md (files-touched list for adding a capability), and a `docs/design/composable-core.md` decision record; example snippet of a custom box `main.rs`
