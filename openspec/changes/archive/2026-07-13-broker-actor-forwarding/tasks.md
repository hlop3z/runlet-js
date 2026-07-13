## 1. Extend the WireInit contract

- [x] 1.1 Add `actor: Option<String>` to `WireInit` (`crates/runlet-wire/src/wire.rs`), placed after `tenant`, with `#[serde(default, skip_serializing_if = "Option::is_none")]` and a doc comment (trusted acting subject, bare subject, `None` on the single-tenant/loopback path, trusted-only).
- [x] 1.2 Add `.field("actor", &self.actor)` to the hand-written `WireInit` `Debug` impl (not a secret — print plainly, like `tenant`).

## 2. Populate the actor on every broker-session handshake

- [x] 2.1 Add an `actor: Option<&str>` param to `wire_init(...)` (`crates/runlet/src/handler/types.rs`); set `actor: actor.map(str::to_owned)` in the returned `WireInit`. Updated the helper doc comment.
- [x] 2.2 In `handler/mod.rs`, pass the actor from the same `identity` used for the box-direct header (`identity.as_ref().and_then(|id| id.user.as_deref())`).
- [x] 2.3 In `handler/lifecycle.rs`, add an `actor` local mirroring the `tenant` local and pass it to `wire_init`.
- [x] 2.4 In `handler/batch_items.rs`, do the same at the batch-item build site.

## 3. Tests

- [x] 3.1 Update the `wire_init(...)` call in `handler/request_io_tests.rs` for the new param; assert the actor lands on `WireInit` when present (`wire_init_carries_flat_resources`) and is absent when `None` (new `wire_init_omits_identity_when_absent`).
- [x] 3.2 Update the `WireInit { … }` struct literals (`broker.rs` + `wire.rs` round-trip tests) for the new field.
- [x] 3.3 Add a `runlet-wire` serde test (`actor_is_additive_and_round_trips`): a `WireInit` with `actor: None` serializes without the `actor` (or `tenant`) key — the `skip_serializing_if` back-compat guarantee — and one with `actor: Some(..)` round-trips.

## 4. Docs

- [x] 4.1 `docs/design/multitenant-trust.md`: broadened the egress-identity subsection ("Identity on the egress paths") — the actor rides `WireInit` on the broker path too, so forwarding is transport-independent (box-direct header ⇔ broker handshake), exactly as the tenant.
- [x] 4.2 `docs/design/resource-egress.md`: the box-direct intro note now lists `X-Runlet-Actor` beside `X-Runlet-Tenant` as the analogues of `WireInit.actor` / `WireInit.tenant`.

## 5. Verify

- [x] 5.1 Run the Docker build + `cargo clippy` gate; clean across runlet-wire / runlet-core / runlet (only pre-existing `runlet-bench` warnings, untouched).
- [x] 5.2 Run `cargo fmt --all --check` (clean) and `cargo test -p runlet-wire -p runlet` — runlet 102 passed, runlet-wire 13 passed, 0 failed.
- [ ] 5.3 `/opsx:sync` the delta into `openspec/specs/tenant-egress/spec.md`, then `/opsx:archive` (use `--skip-specs` on archive since sync already applied the delta).
