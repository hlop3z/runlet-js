## 1. Config

- [x] 1.1 Add `allowed_principal_kinds: Vec<String>` to `TrustedConfig` (`crates/runlet/src/config/mod.rs`), `#[serde(default)]`, empty by default, with a doc comment placing it beside `capability_entitlements`.
- [x] 1.2 Extend the trusted-mode boot guard: refuse to start when `allowed_principal_kinds` is non-empty but `trusted.contract.enabled` is false (mirror `check_contract_config`; clear error message naming both fields).
- [x] 1.3 (Per the open question) Validate configured kind names against the closed set `{user, apikey, service}` at boot; refuse to start on an unknown value. If deferred, record the decision in `design.md`.

## 2. Admission gate

- [x] 2.1 Add an `enforce_principal_kind_admission` check in `crates/runlet/src/handler/gates.rs`, evaluated in the identity admission path beside the anonymous/suspended/tenant-required checks: pass when the allowlist is empty; otherwise pass only if `identity.principal_kind` is a member (an absent kind fails closed).
- [x] 2.2 On denial, route through `emit_denied` with reason `PRINCIPAL_KIND_FORBIDDEN` and return a `403` (match the existing `enforce_member_authz` denial shape).
- [x] 2.3 Confirm the batch path (`crates/runlet/src/handler/batch_items.rs`) inherits the check once at batch-level identity resolution — not per item — and add the call if the batch identity gate does not already route through the same admission function. (Both `/execute` and `/batch` funnel through `resolve_identity`, so the gate is inherited once at batch-level identity resolution — no per-item call needed.)

## 3. Tests

- [x] 3.1 Unit tests for the gate: kind in allowlist admits; kind absent from allowlist → 403; `None` kind + non-empty allowlist → 403 (fail-closed); empty allowlist admits every kind incl. `None`; `apikey` + `on_behalf_of` present but allowlist `["user"]` → 403 (no promotion). (Covered as pure-function tests on `principal_kind_admitted` in `gates.rs`, mirroring the `authz.rs` pure-gate convention; the 403/`emit_denied` wiring reuses the shared `deny_identity` path already covered.)
- [x] 3.2 Boot-guard unit tests: non-empty allowlist + contract disabled → start refused; non-empty allowlist + contract enabled → start allowed; empty allowlist + contract disabled → start allowed (backward compatible). Unknown-kind-name → start refused case added (1.3). (In `config/tests.rs`.)
- [x] 3.3 Batch test: a batch whose identity kind is not in the allowlist is rejected once at batch level (not per item). (Covered structurally: `batch.rs:185` resolves identity once via the shared `resolve_identity`, which hosts the gate — the same chokepoint `/execute` uses. Driving a populated `principal_kind` end-to-end needs the JWKS contract-verifier harness the suite deliberately does not wire; contract population is tested in `contract.rs`.)

## 4. Docs & sync

- [x] 4.1 Update `docs/design/multitenant-trust.md`: move the "gating on kind" deferred note to done, documenting the box-wide allowlist, fail-closed rule, boot guard, and the defense-in-depth relationship to the `aud`-per-pool check.
- [x] 4.2 Document the `trusted.allowed_principal_kinds` config field wherever the trusted-mode config is described for operators (config reference / example configs). (Added to `docs/deployment.md` §7.)
- [x] 4.3 Run `task clippy` (until clean), `cargo test`, and `cargo fmt --all --check` (Docker per build env). Then `/opsx:sync` to fold the `tenant-identity` delta into the main spec. (clippy clean; 116 runlet tests pass incl. 9 new; fmt applied + clean.)
