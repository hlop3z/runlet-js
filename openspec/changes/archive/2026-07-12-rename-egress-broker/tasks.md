## 1. Config surface (keys + struct)

- [x] 1.1 In `crates/runlet/src/config/mod.rs`: rename field `fabricd_socket` → `broker_socket` (decl ~L93, `Default` ~L170) and update its doc comment (L85–91)
- [x] 1.2 In `crates/runlet/src/config/mod.rs`: rename field `fabricd_quic` → `broker_quic` (decl ~L99, `Default` ~L171) and update its doc comment (L94–97)
- [x] 1.3 In `crates/runlet/src/config/mod.rs`: rename struct `FabricdQuic` → `BrokerQuic` (decl ~L417) and its doc comment (L409–414); leave nested fields (`replicas`/`server_name`/`server_cert_pin`/`auth_token`/`auth_token_file`) unchanged
- [x] 1.4 Confirm NO `#[serde(alias)]` is added (greenfield hard rename, per design Decision 2); keep `#[serde(default)]` / `#[serde(deny_unknown_fields)]` as-is

## 2. Broker transport module (rename `sidecar.rs` → `broker.rs`)

- [x] 2.1 `git mv crates/runlet/src/sidecar.rs crates/runlet/src/broker.rs`
- [x] 2.2 In `crates/runlet/src/main.rs`: change `mod sidecar;` (L10) → `mod broker;`
- [x] 2.3 In `broker.rs`: rename enum `SidecarTransport` → `BrokerTransport` (decl L47, `impl` L57, param L309, match arms `None`/`Uds`/`Quic` L313/317/324); rename struct `SidecarEgress` → `BrokerEgress` (decl L365, `impl` L375, `impl Egress` L390, `impl MeteredEgress` L424)
- [x] 2.4 In `broker.rs`: update `use crate::config::FabricdQuic;` (L42) → `BrokerQuic`, and the `FabricdQuic` param types in `from_config` (L69, L165) and `build_auth` (L471)
- [x] 2.5 In `broker.rs`: update user-facing string literals/messages that name the old keys — `"fabricd_quic.replicas must not be empty"` (L73), `"fabricd_socket (UDS) is unsupported…; use fabricd_quic"` (L80), `"set only one of fabricd_quic.auth_token / auth_token_file"` (L474), the `"no fabricd egress sidecar configured"` message (L314), and the seam doc-comments (module header L1/11/14, L48/247/302/484, L58/490) → broker wording
- [x] 2.6 In `broker.rs` tests (L487–509): update `use super::{… SidecarTransport …}` and `SidecarTransport::{from_config,None}` references to `BrokerTransport`

## 3. Consumers of the renamed types

- [x] 3.1 `crates/runlet/src/main.rs`: `use crate::sidecar::SidecarTransport;` (L39) → `use crate::broker::BrokerTransport;`; `SidecarTransport::from_config(` (L169) → `BrokerTransport::…`; `config.fabricd_socket`/`config.fabricd_quic` (L170–171) → `broker_socket`/`broker_quic`; log/comment strings "no fabricd egress sidecar" / "fabricd egress sidecar configured" (L166/174/175) → broker wording
- [x] 3.2 `crates/runlet/src/handler/types.rs`: `use crate::sidecar::SidecarTransport;` (L24) → `crate::broker::BrokerTransport`; field `transport: SidecarTransport` (L52) + doc `SidecarTransport::None` (L50) → `BrokerTransport`; reword the `SidecarTransport::None ⇒ …` doc (L47–52) seam prose
- [x] 3.3 `crates/runlet/src/handler/mod.rs`: `use crate::sidecar::{SessionConn, SidecarEgress, connect_session};` (L25) → `crate::broker::{SessionConn, BrokerEgress, connect_session}`; `SidecarEgress::new(` (L390) → `BrokerEgress::new(`; reword doc (L385)
- [x] 3.4 `crates/runlet/src/local_io.rs`: `use crate::sidecar::SidecarEgress;` (L30) → `crate::broker::BrokerEgress`; `broker: Option<Arc<SidecarEgress>>` field/param (L72, L85) → `BrokerEgress`; doc `[`SidecarEgress`]` (L7) → `[`BrokerEgress`]`
- [x] 3.5 Update remaining `use crate::sidecar::…` module-path imports (role-neutral symbols, path only): `handler/lifecycle.rs:19`, `handler/response.rs:20`, `handler/batch_items.rs:23`, `handler/fail_closed_envelope_tests.rs:5` → `crate::broker::…`

## 4. Tests

- [x] 4.1 Update unit-test `use`/refs of `SidecarTransport` in `handler/batch_tests.rs` (L9, L50), `handler/execute_status_tests.rs` (L8, L50), `handler/trusted_pipeline_tests.rs` (L13, L54) → `BrokerTransport` (+ path `crate::broker`)
- [x] 4.2 `tests/stress_test.py` (L162–164) and `tests/stress_breaker_esm.py` (L168–169): change the generated `config.json` key `"fabricd_socket"` → `"broker_socket"`
- [x] 4.3 Reword `fabricd sidecar` seam prose in Python docstrings/comments: `tests/test_simple.py` L451/453/1208/1223 (comments only — harness writes no keys)

## 5. Spec main-file prose (non-requirement text)

- [x] 5.1 `openspec/specs/tenant-egress/spec.md`: reword the Purpose section (L5–7) — "box↔`fabricd` session handshake" → "box↔broker session handshake" (the requirement deltas are handled by `/opsx:sync`; this is Purpose prose only)
- [x] 5.2 Confirm `observability` (`fabricd_db_breaker_trips_total`) and `tenant-metering` ("fabricd-drained egress") specs are intentionally left unchanged (daemon's own metrics/behavior — design Decisions 3 & 4)

## 6. User-facing docs sweep (reword seam → broker; keep external-product references)

- [x] 6.1 `CLAUDE.md`: reword seam/topology + config-key mentions (L70, L114, L115, L141) to "egress broker (reference impl: `fabricd`)"; keep the `../fabricd` checkout reference (L159)
- [x] 6.2 `README.md`: reword L10, L65 ("sidecar"→"broker"), L232–233 + L900–901 (config keys `fabricd_socket`/`fabricd_quic`), L246, L496, L875 (example key); keep external-product refs (L207 `fabricd.json`/`FABRICD_CONFIG`; "remote fabricd" the product)
- [x] 6.3 `docs/deployment.md`: reword seam wording + config keys (L8, L50, L65, L193/194/198/202/216/222/231/239); keep `fabricd`'s own `resources` config / `fabricd.json` / `FABRICD_*` references
- [x] 6.4 `docs/99-errors.md` (L175–176), `docs/03-capabilities.md`, `docs/README.md`: reword "egress sidecar"/"sidecar" seam prose → broker
- [x] 6.5 `docs/design/resource-egress.md`, `composable-core.md`, `network-fabric.md`, `resilience.md`: reword the mux-fallback/seam/topology wording + config keys → broker; **keep** two-binary/two-process architecture references that name the `fabricd` binary/crate as a distinct process, and keep the unrelated "identity sidecar" (nexus) mentions in `multitenant-trust.md`/`nexus-upstream-requirements.md`
- [x] 6.6 `container/README.md` (L102/106/114/115) and `crates/runlet/Cargo.toml` (dependency comments L19, L39): reword seam prose → broker
- [x] 6.7 `container/docker-compose.yml`: reword only the this-repo config key in the commented example (L20 `"fabricd_socket"` → `"broker_socket"`); keep the external image build, `FABRICD_CONFIG`/`FABRICD_SOCKET` env, service name `fabricd`, and `fabricd-sock` volume

## 7. Prose-only sweep of core/wire doc-comments (low priority; no code/type changes)

- [x] 7.1 `runlet-core` illustrative doc-comments naming "(a `fabricd` sidecar)" as an example wired `Egress` → "broker": `CONSUMER_NOTES.md:174`, `capability.rs` (L114/232/298/332), `host.rs` (L12/128/301/512), `engine/types.rs:211`, `egress.rs`
- [x] 7.2 `runlet-wire` doc-comment seam prose ("behind a sidecar", "round-trips a sidecar (`fabricd`) call") → broker: `egress.rs` (L10/46), `errors.rs` (L11/95), `wire.rs:9` — **prose only; do not touch protocol bytes/struct fields**

## 8. Verify (Docker — native build blocked by WDAC/aws-lc-sys)

- [x] 8.1 Grep the repo for residual `fabricd_socket`/`fabricd_quic`/`FabricdQuic`/`SidecarEgress`/`SidecarTransport`/`crate::sidecar`/`mod sidecar` — expect zero outside `openspec/changes/archive/**` (immutable history) and legitimate external-product references
- [x] 8.2 `cargo build` (whole workspace) in Docker — compiles clean after the rename
- [x] 8.3 `task clippy` in Docker — clean (the strict gauntlet is not enforced by `cargo build`; re-run until no errors)
- [x] 8.4 `cargo fmt --all --check` — passes (fmt is not enforced by clippy/build)
- [x] 8.5 `cargo test` in Docker — unit tests pass, including the fail-closed egress invariant tests in `broker.rs` + `handler.rs`
- [x] 8.6 Run the Python harness (`python tests/test_simple.py`) with `task test-backends-up` — box-only suite green
- [x] 8.7 Run `/opsx:sync` to fold the `tenant-egress` + `capability-registry` requirement deltas into the main specs
