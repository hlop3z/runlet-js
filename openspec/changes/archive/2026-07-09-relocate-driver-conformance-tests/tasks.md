## 1. Survey before deletion

- [x] 1.1 List every layer-C section + helper to remove in `tests/test_simple.py`: `test_db_engine`, `test_nats`, `test_pgbouncer_edges`, `test_pooler_query_timeout`, `test_auth_provider`, `test_circuit_breaker`, `test_statement_timeout_clamp`, plus `_locate_fabricd`, `_full_resources`, `_fabricd_up`, identity-provider mint helpers, and per-section reachability probes.
- [x] 1.2 For each helper slated for deletion, grep for remaining callers among the *kept* (layer-A / box-direct) sections so no live section is orphaned.
- [x] 1.3 Confirm `_start_servers` no longer needs to build/launch fabricd once the C sections are gone; note what the kept sections still require (server with `debug: true`, scripts_dir, local httpbin, box-direct `local_resources`).

## 2. Add the fail-closed test (close the real gap)

- [x] 2.1 Inspect `crates/runlet/src/sidecar.rs` session-open path: determine whether the "no sidecar + no box-direct binding ⇒ Unavailable/503 EGRESS_UNAVAILABLE" decision is reachable as a unit-testable function (resolves design Open Question / D4).
- [x] 2.2 If unit-testable: add a `#[cfg(test)]` test asserting an `io`-addressing session with no configured backend yields the `Unavailable` outcome that maps to `503 EGRESS_UNAVAILABLE`.
- [x] 2.3 (N/A — the decision WAS cleanly unit-testable, see 2.2) If not cleanly unit-testable: add a minimal `/execute` assertion in `tests/test_simple.py` (name in `config.io`, no sidecar, no box-direct binding ⇒ `503` with code `EGRESS_UNAVAILABLE`), and one asserting the refusal is `503` not `429`/`4xx`.
- [x] 2.4 Verify the fail-closed test covers the four spec scenarios (no backend, retryable, precedes admission, non-egress unaffected) — at minimum the no-backend + retryable + non-egress-unaffected cases are assertable here.

## 3. Delete layer C and its scaffolding

- [x] 3.1 Remove the layer-C section functions listed in 1.1 from `tests/test_simple.py`.
- [x] 3.2 Remove the fabricd scaffolding: `_locate_fabricd`, `_full_resources`, `_fabricd_up` flag machinery, identity-provider helpers, and per-section probes; delete the sibling-checkout build step and `FABRICD_BIN` lookup.
- [x] 3.3 Remove now-dead imports / constants / `main()` registrations for the deleted sections; ensure `main()` runs a coherent kept-section set.
- [x] 3.4 Remove fabricd-only fixtures no longer referenced (e.g. resource-table blobs, provider env plumbing) if not used by kept sections.

## 4. Docs sweep

- [x] 4.1 Update `CLAUDE.md` integration-test notes: drop `FABRICD_BIN` / `../fabricd` sibling-checkout requirement and the "driver-backed sections self-skip" language for the deleted sections; state that real-driver conformance now lives in the fabricd repo.
- [x] 4.2 Update any `docs/design/*` (e.g. resource-egress / pooled-capabilities / resilience / network-fabric) references that point at the deleted test sections as their coverage.
- [x] 4.3 Add a short hand-off pointer (design doc or the tenant-egress-adjacent doc) recording that fabricd owns the real-driver conformance suite against the `runlet-wire` types (design D5).

## 5. Verify

- [x] 5.1 Run the trimmed `tests/test_simple.py` (Docker per build-env rules) with **no** fabricd present; confirm all kept sections pass and none self-skip for a missing fabricd.
- [x] 5.2 Run `cargo test` (Docker) — the new fail-closed unit test passes; the existing `runlet-wire` `wire.rs` encoding tests still pass.
- [x] 5.3 Run `task clippy` (Docker) — clean under the lint gauntlet (re-run until truly clean).
- [x] 5.4 Confirm net line count in `tests/test_simple.py` decreased and no reference to `FABRICD_BIN` / `_locate_fabricd` / `../fabricd` remains in the repo (grep).
