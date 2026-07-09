## Why

The box repo's integration suite (`tests/test_simple.py`) still carries ~7 driver-conformance
sections (db, nats, pgbouncer, pooler-timeout, auth, circuit-breaker, statement-timeout-clamp)
that test **fabricd's** behavior *through* the box. They force a cross-repo build dependency
(`_locate_fabricd` builds/locates a sibling `../fabricd` checkout) plus ~46 lines of probe/
self-skip scaffolding — and several already self-skip permanently because the behavior they
assert *moved to fabricd and isn't implemented there yet* (`circuit_breaker`, `statement_timeout_clamp`).
Those tests assert nothing here today. This contradicts the repo's own contract: this repo owns
the wire contract (`runlet-wire`) and fabricd is the replaceable conformer — so conformance is
fabricd's burden to prove in *its* repo, not ours to prove against it.

The goal is net code **reduction**: delete the emigrant tests and their scaffolding, and close
the one real gap they were hiding — the box's fail-closed behavior (no egress backend ⇒
`503 EGRESS_UNAVAILABLE`) is never positively asserted anywhere today.

## What Changes

- **Remove** the driver-conformance sections from `tests/test_simple.py`: `test_db_engine`,
  `test_nats`, `test_pgbouncer_edges`, `test_pooler_query_timeout`, `test_auth_provider`,
  `test_circuit_breaker`, `test_statement_timeout_clamp`, and their identity-provider helpers.
- **Remove** the fabricd test scaffolding: `_locate_fabricd`, `_full_resources`, the `_fabricd_up`
  flag machinery, per-section reachability probes, and the sibling-checkout build step. This
  severs the test suite's dependency on a `../fabricd` checkout / `FABRICD_BIN`.
- **Add** a single positive fail-closed assertion: a request that names an `io` resource with no
  sidecar and no box-direct binding configured returns `503 EGRESS_UNAVAILABLE`. Lowest-boilerplate
  home is a Rust unit test on `sidecar.rs`'s session-open path (no socket, no process); a thin
  `/execute` assertion is the alternative.
- **Do NOT** add a fake fabricd peer or a separate contract kit. The wire *encoding* is already
  unit-tested in `runlet-wire` (`wire.rs` `mod tests`); the happy-path round-trip is exercised for
  real by fabricd's own integration suite; and `runlet-wire`'s serde types **are** the contract, so
  fabricd tests against them directly in its repo. Publishing a second contract artifact is exactly
  the boilerplate this change avoids.
- **Hand-off note** to the fabricd repo (docs / this change's design): the real-driver e2e coverage
  now lives there, asserted against the `runlet-wire` types it already depends on.
- Update `CLAUDE.md` and `docs/design/*` references that describe the deleted test sections /
  `FABRICD_BIN` / `../fabricd` sibling-checkout requirement for the suite.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `tenant-egress`: add a **fail-closed** requirement — when a request addresses an allowlisted `io`
  logical name but no egress backend (neither a box-direct `local_resources` binding nor a
  configured/reachable `fabricd` sidecar) can serve it, the box SHALL refuse with a retryable
  `503 EGRESS_UNAVAILABLE` before executing egress, and MUST NOT fall back to any ambient path.
  This promotes an invariant currently stated only in prose into a testable requirement.

## Impact

- **Tests**: `tests/test_simple.py` shrinks (7 sections + ~46 lines of scaffolding removed);
  the suite no longer needs a fabricd binary or a `../fabricd` sibling checkout. Remaining sections
  (box-only behavior + box-direct `local_io`) are unaffected.
- **New test**: one fail-closed unit test (`crates/runlet/src/sidecar.rs`) or `/execute` assertion.
- **Docs**: `CLAUDE.md` (Commands / integration-test notes on `FABRICD_BIN`, sibling checkout,
  self-skipping driver sections) and any `docs/design/*` that reference the deleted sections.
- **No production code behavior change** — the fail-closed path already exists; this only adds a
  test and promotes it to a spec requirement.
- **Cross-repo**: fabricd (sibling repo `github.com/hlop3z/fabricd`) is expected to grow / already
  owns the real-driver conformance suite against the `runlet-wire` contract. No change to
  `runlet-wire` itself (contract unchanged; encoding tests stay).
