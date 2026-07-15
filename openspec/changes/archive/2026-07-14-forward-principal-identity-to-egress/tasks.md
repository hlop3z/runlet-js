## 1. Wire contract (`runlet-wire`)

- [x] 1.1 Added `principal_kind: Option<String>` and `on_behalf_of: Option<String>` to `WireInit` (`crates/runlet-wire/src/wire.rs`), each `#[serde(default, skip_serializing_if = "Option::is_none")]`, documented; also added both to the manual `Debug` impl (trusted-edge identity, printed plainly)
- [x] 1.2 Wire round-trip test extended (`wire.rs`): absent fields serialize to nothing (byte-minimal single-tenant handshake); a verified api-key contract (`actor`=key id + `principal_kind` + `on_behalf_of`) round-trips all three

## 2. Box-direct egress (`local_io.rs`)

- [x] 2.1 Added header consts `PRINCIPAL_KIND_HEADER = "x-runlet-principal-kind"` and `ON_BEHALF_OF_HEADER = "x-runlet-on-behalf-of"`; added `principal_kind`/`on_behalf_of` fields to `BoxEgress`
- [x] 2.2 `build_request` attaches each header only when its field is `Some`, with the same guard as tenant/actor; the `{action, payload}` body is untouched (asserted identical)
- [x] 2.3 Extended `BoxEgress::new` to accept the two values; added an `egress_full` test helper (kept `egress_with` for existing tests via `None`/`None`), plus tests: verified api-key adds both headers alongside actor with the body identical; plain path omits both

## 3. Thread from identity at the build sites (`handler`)

- [x] 3.1 Extended the `wire_init` helper (`handler/types.rs`) with `principal_kind` + `on_behalf_of` params (+ `#[expect(too_many_arguments, ...)]`, matching `BoxEgress::new`'s pattern) and set them on the built `WireInit`
- [x] 3.2 Broker build sites: `handler/mod.rs` (single-execute), `batch_items.rs`, and `lifecycle.rs` all pass `identity.principal_kind`/`on_behalf_of` into `wire_init(...)`
- [x] 3.3 Box-direct build sites: added `principal_kind`/`on_behalf_of` to the `ExecuteBlocking` struct + its destructure + the `BoxEgress::new` call; populated from identity at all three `ExecuteBlocking {}` construction sites (execute / batch / lifecycle), sourced only from the trusted extractor

## 4. Docs + specs

- [x] 4.1 Updated `docs/design/multitenant-trust.md` "Identity on the egress paths": documents `X-Runlet-Principal-Kind` / `X-Runlet-On-Behalf-Of` + the broker `WireInit` fields, verified-contract-only, `actor` unchanged; flipped the old "not forwarded" note and the sub-mode "Open (deferred)" line (forwarding now done; only gating deferred)
- [x] 4.2 `tenant-egress` delta matches the implemented field/header names (`principal_kind`/`on_behalf_of`; `x-runlet-principal-kind`/`x-runlet-on-behalf-of`)

## 5. Verify (Docker-only)

- [x] 5.1 `cargo build -p runlet -p runlet-wire` + full-workspace `cargo clippy` clean; `cargo fmt --all --check` clean
- [x] 5.2 `cargo test -p runlet -p runlet-wire` green — 107 + 13 pass, including the new box-direct header tests and the wire round-trip
- [x] 5.3 `cargo tree -i ring` still empty (no new deps); supply-chain (vet/deny) unaffected — no dependency change
