# Tasks: composable-capability-core

## 1. Registry + mux (behavior-preserving)

- [ ] 1.1 Add `CapabilityDef` (name, js_wrapper, trust: `OperatorSupplied | ScriptControlled(SsrfPolicy)`, backend: `Arc<dyn Egress>`) to runlet-core; reject duplicate names at build
- [ ] 1.2 Add `LogicHost::builder()` (capabilities + optional `fallback_egress`); keep the old constructor delegating to it temporarily
- [ ] 1.3 Replace the single egress slot with the per-name mux (`HashMap<name, Arc<dyn Egress>>` + fallback) in `ExecParams`/`engine.rs::io_hook`; preserve `EGRESS_UNAVAILABLE` semantics
- [ ] 1.4 Route `inject_apis` from the registered def list instead of cfg blocks (standard defs registered internally for now — engine tests must pass unchanged)
- [ ] 1.5 Enforce D4: apply the SSRF guard inside the mux for `ScriptControlled` defs; unit test a script-controlled def hitting a private IP is rejected pre-connect
- [ ] 1.6 Unit tests: mixed topology (one name local `EchoEgress`, one via fallback), unregistered name → global undefined, deterministic profile injects nothing

## 2. Metrics + config dynamization

- [ ] 2.1 Replace cfg'd `ExecMetrics`/`Collectors` per-cap fields with a name-keyed collector map; drain into `Outcome`
- [ ] 2.2 Handler: replace fixed `Meta.<cap>_requests` with `meta.io.<name>` map; make `RequestConfig.io` generic over registered names
- [ ] 2.3 Verify `events.rs` usage-event dims against the new metrics source (no tenant-metering spec change)
- [ ] 2.4 Update `tests/test_simple.py` meta assertions and `container/types.d.ts` meta typings to `meta.io`

## 3. Preset extraction + core slimming

- [ ] 3.1 Create `runlet-caps` crate: the six standard `CapabilityDef`s (JS wrappers moved from core, trust declarations, action-token fixture test vs fabric-backends names)
- [ ] 3.2 Flip the `runlet` bin to compose the preset + sidecar fallback (stock behavior parity)
- [ ] 3.3 Delete core's six wrapper modules + `js/*.js` for driver caps; remove the six cargo features; keep `http`/`s3`
- [ ] 3.4 Update `scripts/sweep.sh` to the `http`/`s3`-only matrix; confirm `--no-default-features` links no network dep (`cargo tree` check)

## 4. Verification + docs

- [ ] 4.1 Full gate: `task check` (fmt, clippy gauntlet, unit tests) + feature sweep
- [ ] 4.2 Integration suite vs sibling fabricd, plus a mixed-topology smoke (one capability bound in-process via `fabric-backends`, rest via sidecar fallback)
- [ ] 4.3 runlet-bench: confirm no measurable hot-path regression from the mux lookup
- [ ] 4.4 Update README (capability pattern + extension how-to), CLAUDE.md (files-touched list for adding a capability), and a `docs/design/composable-core.md` decision record; example snippet of a custom box `main.rs`
