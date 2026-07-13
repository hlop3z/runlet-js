## 1. Thread the actor subject into box-direct egress

- [x] 1.1 Add an `actor: Option<String>` param to `BoxEgress::new` (`crates/runlet/src/local_io.rs`) and store it as a struct field with a doc comment (trusted-only, bare subject, `None` on the single-tenant path). Add an `ACTOR_HEADER` const (`"x-runlet-actor"`) beside `TENANT_HEADER`; extend the `#[expect(clippy::too_many_arguments, …)]` reason.
- [x] 1.2 In `build_request` (`local_io.rs`), attach the `X-Runlet-Actor` header when the stored actor is `Some`, alongside the existing `X-Runlet-Tenant` logic; add no header when `None`. Keep the `LocalCallEnvelope { action, payload }` body unchanged (D9). _(Reshaped `build_request` from a `match` to sequential `if let` guards so two optional headers compose cleanly.)_
- [x] 1.3 Add an `actor: Option<String>` field to the `ExecuteBlocking` params struct (`crates/runlet/src/handler/mod.rs`) and pass it into `BoxEgress::new` inside `execute_blocking`.

## 2. Populate the actor at every build site

- [x] 2.1 In `handler/mod.rs` (the primary `/execute` site), set `ExecuteBlocking.actor` from `identity.as_ref().and_then(|id| id.user.as_deref().map(str::to_owned))`, mirroring the adjacent `tenant` line.
- [x] 2.2 In `handler/lifecycle.rs`, do the same at that build site.
- [x] 2.3 In `handler/batch_items.rs`, do the same at the batch-item build site.

## 3. Tests

- [x] 3.1 Add a `local_io.rs` unit test: a box-direct call with an actor present emits `X-Runlet-Actor` and the body is still exactly `{action, payload}` (`actor_present_adds_header_and_leaves_body_identical`). _(Generalized the helper `egress_with_tenant` → `egress_with(tenant, actor)` and factored `header`/`body_bytes` helpers.)_
- [x] 3.2 Add a `local_io.rs` unit test: a box-direct call with actor `None` emits no `X-Runlet-Actor` header (`no_actor_omits_header`).
- [x] 3.3 Add a `local_io.rs` unit test: tenant and actor both present emit both headers with the body unchanged (`tenant_and_actor_ride_together`).
- [x] 3.4 Confirm `http`/`s3` paths are untouched — the actor is threaded only into `BoxEgress`, so those paths carry no identity by construction.

## 4. Docs

- [x] 4.1 `docs/design/multitenant-trust.md`: in the "Tenant is the universal key" area, add the cross-repo vocabulary equivalence (nexus `Workspace` == runlet `tenant` == event-logs `Tenant`, propagated verbatim) with a pointer to nexus's `tenancy-and-identity.md` as canonical; add a one-line note on the identity axes the box keys on (Workspace/tenant), carries-but-does-not-read (Actor/subject), and never sees (Account).
- [x] 4.2 `docs/design/multitenant-trust.md`: add a short subsection on box-direct egress identity — it carries `X-Runlet-Tenant` (where) and now `X-Runlet-Actor` (who, bare subject); principal kind is deliberately not forwarded (crypto-gated, no crypto in the box); a future kind would be a separate trusted header.
- [x] 4.3 `docs/design/multitenant-trust.md:54`: drop the stale "a ZITADEL org; solo users get a personal workspace" characterization in favor of the box's opacity doctrine (the acting workspace, opaque to the box).
- [x] 4.4 `docs/design/nexus-upstream-requirements.md:33`: drop the "selected/authorized via a ZITADEL org-scoped token + grants" characterization the same way (nexus is IdP-agnostic; the box treats the value as opaque).

## 5. Verify

- [x] 5.1 Run the Docker build + `cargo clippy` gate (native cargo is WDAC-blocked); fix any lint findings until clean. _(`cargo clippy -p runlet` + workspace clippy clean; the only warnings are pre-existing `runlet-bench/sweep.rs`, untouched.)_
- [x] 5.2 Run `cargo fmt --all --check` and `cargo test -p runlet`. _(fmt clean after reflowing the `header` test helper; 101 passed / 0 failed, incl. the 5 `local_io` header tests.)_
- [ ] 5.3 `/opsx:sync` the delta into `openspec/specs/tenant-egress/spec.md`, then `/opsx:archive`.
