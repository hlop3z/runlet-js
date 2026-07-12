## 1. Thread the tenant into box-direct egress

- [x] 1.1 Add a `tenant: Option<String>` param to `BoxEgress::new` (`crates/runlet/src/local_io.rs`) and store it as a struct field with a doc comment (trusted-only, `None` on the single-tenant path).
- [x] 1.2 In `call_local` (`local_io.rs`), attach an `X-Runlet-Tenant` header to the box-direct POST when the stored tenant is `Some`; add no header when `None`. Keep the `LocalCallEnvelope { action, payload }` body unchanged (D9). _(Extracted a `build_request` helper so the header logic is unit-testable without a network round-trip.)_
- [x] 1.3 Add a `tenant: Option<String>` field to the `ExecuteBlocking` params struct (`crates/runlet/src/handler/mod.rs`) and pass it into `BoxEgress::new` inside `execute_blocking`. _(`BoxEgress::new` now carries an `#[expect(clippy::too_many_arguments, reason=…)]` — 6 args, called once.)_

## 2. Populate the tenant at both build sites

- [x] 2.1 In `handler/lifecycle.rs`, set `ExecuteBlocking.tenant` from the same `identity.and_then(|t| t.tenant.as_deref())` value already computed for `wire_init` (own the string for the params struct).
- [x] 2.2 In `handler/batch_items.rs`, do the same for the batch-item build site. _(Also the primary `/execute` site in `handler/mod.rs` — three build sites total, not two.)_

## 3. Tests

- [x] 3.1 Add a `local_io.rs` unit test: a box-direct call with a tenant present emits `X-Runlet-Tenant` and the body is still exactly `{action, payload}` (`tenant_present_adds_header_and_leaves_body_identical`).
- [x] 3.2 Add a `local_io.rs` unit test: a box-direct call with tenant `None` emits no such header (`no_tenant_omits_header`).
- [x] 3.3 Confirm `http`/`s3` paths are untouched — no code changed in `http.rs`/`s3.rs`; the tenant is threaded only into `BoxEgress`, so those paths carry no identity by construction.

## 4. Verify

- [x] 4.1 Run the Docker build + `cargo clippy` gate (native cargo is WDAC-blocked); fix any lint findings until clean. _(clean after the `#[expect]`.)_
- [x] 4.2 Run `cargo fmt --all --check` and `cargo test -p runlet` — 98 passed, fmt clean. _(The box-only Python integration suite exercises no trusted-mode box-direct `local_resources` tenant scenario, so this change is covered by the Rust unit tests, matching how the fail-closed egress invariant is unit-tested rather than in the Python harness.)_
- [x] 4.3 `/opsx:sync` the delta into `openspec/specs/tenant-egress/spec.md`, then `/opsx:archive`.
