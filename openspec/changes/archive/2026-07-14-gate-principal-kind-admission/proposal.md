## Why

The verified `principal_kind` (`user`/`apikey`/`service`) is already extracted from the signed
identity contract and forwarded downstream, but nothing consumes it for authorization — a `service`
caller and a human `user` with the same entitlements are admitted identically. Operators running the
box behind nexus want to dedicate an instance to one class of principal (a user-box, a service-box)
so the two scale independently and a mis-routed token cannot execute. `docs/design/multitenant-trust.md`
already parks this as the deferred *"gating on kind"* concern.

## What Changes

- Add a box-wide **principal-kind admission allowlist**: `trusted.allowed_principal_kinds`
  (a list of kind names). Empty (the default) preserves today's behavior — every kind is admitted.
- When the list is non-empty, admit a request **only if** its verified `principal_kind` is a member;
  otherwise reject with `403 PRINCIPAL_KIND_FORBIDDEN`.
- **Fail closed:** a request whose `principal_kind` is `None` (plain-header mode, or a contract that
  did not populate the kind) is never a member of a non-empty allowlist → denied.
- Add a **boot guard**: a non-empty `allowed_principal_kinds` requires `trusted.contract.enabled`,
  because kind is only ever populated by a verified contract; without it the box would deny every
  request. The box refuses to start on this misconfiguration (mirroring the existing contract boot guard).
- The gate is a single set-membership check at **caller admission** — beside the anonymous / suspended /
  tenant-required checks — evaluated **once per request** (and once per `/batch`, since identity is
  resolved at batch level). It is **not** on the per-capability path; `authz.rs` and the coarse member
  gate are untouched.

Not breaking: default-empty preserves current behavior for every existing deployment.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `tenant-identity`: add a principal-kind admission requirement (box-wide allowlist, fail-closed on an
  absent kind) and extend the boot-guard requirement so a configured allowlist requires the contract
  sub-mode.

## Impact

- **Config:** `crates/runlet/src/config/mod.rs` — new `allowed_principal_kinds: Vec<String>` field on
  `TrustedConfig`; a new arm in the trusted-mode boot guard (`check_contract_config` / sibling).
- **Admission gate:** `crates/runlet/src/handler/gates.rs` — one new membership check in the identity
  admission path, plus a `PRINCIPAL_KIND_FORBIDDEN` denial (audit `emit_denied` + `403`).
- **Batch:** `crates/runlet/src/handler/batch_items.rs` — the batch-level identity gate inherits the
  check (evaluated once, not per item).
- **Tests:** unit tests for the gate (allow / deny / fail-closed-on-None) and the boot guard; Python
  harness coverage in the trusted section if a live contract path is exercised.
- **Untouched:** `authz.rs`, per-capability member authz, `runlet-core`, the wire contract. The edge
  (nexus) and the broker are unaffected — this is a box-local admission decision.
