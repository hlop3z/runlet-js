## Why

This repo owns the egress contract (`runlet-wire`) and treats `fabricd` as a replaceable
reference implementation — "anything that speaks the protocol can stand in for it." Yet the box's
own config surface and internal types name that swappable seam after the reference product
(`fabricd_socket`, `fabricd_quic`, `FabricdQuic`) or after a deployment topology (`SidecarEgress`,
`SidecarTransport`, `sidecar.rs`). That is a branding/topology leak into an abstraction that is
supposed to be implementation-agnostic. "Sidecar" is additionally wrong for the remote QUIC path,
which is a broker on another host, not a co-located sidecar. We are greenfield, so we can rename to
a role-based term with no compatibility burden.

## What Changes

- **BREAKING (config)** Rename operator config keys `fabricd_socket` → `broker_socket` and
  `fabricd_quic` → `broker_quic`. No serde aliases (greenfield; the old keys are removed outright).
- Rename the QUIC config struct `FabricdQuic` → `BrokerQuic`.
- Rename the transport types `SidecarEgress` → `BrokerEgress`, `SidecarTransport` → `BrokerTransport`,
  and the module `sidecar.rs` → `broker.rs` (with its `mod`/`use` paths).
- Reword the WHAT-layer terminology in the affected specs so the seam is named by its **role**
  ("the egress broker"), with `fabricd` demoted to a named reference implementation. Behavior is
  unchanged — this is a terminology/naming delta, not a requirement change.
- Reword prose in `CLAUDE.md`, `docs/`, and code comments from "egress sidecar `fabricd`" to
  "egress broker (reference implementation: `fabricd`)".
- **Explicitly NOT changed** (correctly-named seam / external product): the `Egress` trait, egress
  mux, `EgressError`, `MeteredEgress`, `runlet-wire`, the wire protocol bytes and struct fields
  (`WireInit`/`WireCall`/…), the `EGRESS_UNAVAILABLE` error code, the sibling `fabricd` repo/binary
  and its own daemon-scoped metric names (e.g. `fabricd_db_breaker_trips_total`).

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `tenant-egress`: terminology only — the box↔broker session, transport, and fail-closed behavior
  are described by the **egress broker** role rather than the `fabricd` brand or the "sidecar"
  topology; `fabricd` is named as the reference implementation. No behavioral requirement changes.
- `capability-registry`: terminology only — the wired egress fallback is referred to as "the broker"
  rather than "the sidecar" in requirement prose and scenarios. No behavioral requirement changes.

## Impact

- **Config (breaking):** operator `config.json` files using `fabricd_socket` / `fabricd_quic` must
  switch to `broker_socket` / `broker_quic`. Deterministic / `http` / `s3` / box-direct requests are
  unaffected (they never name the broker transport).
- **Code:** `crates/runlet/src/config/mod.rs` (keys + `FabricdQuic`), `crates/runlet/src/sidecar.rs`
  → `broker.rs`, `crates/runlet/src/handler/types.rs` and any module referencing `SidecarEgress` /
  `SidecarTransport` / `mod sidecar`. Confined to the `runlet` binary crate — `runlet-core` and
  `runlet-wire` are untouched.
- **Tests:** the Python harness generates `.test-run/config.json`; any generated/example config
  keys and Rust unit tests asserting the fail-closed egress invariant must use the new names.
- **Docs:** `CLAUDE.md`, `docs/design/network-fabric.md`, `docs/design/resource-egress.md`, and other
  prose referencing the transport by brand/topology.
- **Wire protocol:** none — the on-the-wire bytes and `runlet-wire` types do not change, so no
  cross-repo coordination with `fabricd` is required.
