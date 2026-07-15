## Why

The box's trusted-identity layer (`crates/runlet/src/identity.rs`) was written against nexus's
pre-hardening header contract and is now stale: nexus's "B-floor trust hardening" **retired** the
bare `x-user-roles` / `x-user-entitlements` / `x-user-suspended` headers and moved those signals
into a **signed `x-identity-contract` (ES256 JWS)**, and now strips the bare headers on every path.
Consequences today when the box runs behind current nexus:

- **Suspension fails open (security defect).** The box reads `x-user-suspended`, which nexus no
  longer sends, so `suspended` parses to `false` — a nexus-suspended principal still executes code.
- **Plan reads the wrong name.** The box reads `x-tenant-plan`; nexus emits `x-workspace-plan`. An
  absent plan drives quota to `most_restrictive()`, throttling every tenant to minimum concurrency.
- **The box would 403 every request.** The box hard-requires `x-tenant-scope: acting`, a header
  nexus never emits; nexus's own box-consumer contract tells boxes to switch to the contract check.
- **The next drift is silent too.** The box checks no contract version, so a future contract change
  breaks it the same invisible way. Nexus already ships the alarm — the `ctr` claim (currently
  `"v1"`) — and the box ignores it.

## What Changes

- Add an **opt-in `trusted.contract` sub-mode**, layered *alongside* the existing generic
  trusted-header path (a non-nexus edge that injects plain headers keeps working — the box stays
  "any trusted edge can stand in"). Contract verification activates only when the operator
  configures it.
- When the sub-mode is enabled, on an identity-enriched request the box **verifies the
  `x-identity-contract` JWS** — JWKS fetched and cached from a configured `jwks_url` (key selected
  by `kid`, refreshed on unknown `kid`), signature verified, and the registered claims checked:
  `iss` equals the configured issuer, `aud` equals this box's configured pool name, `exp` is in the
  future (small skew leeway), and **`ctr` is in the configured supported set** (default `["v1"]`).
- The box **sources identity from the verified claims** — `workspace_id` (tenant), `sub` (user),
  `roles`, `entitlements`, `suspended`, `plan`, plus the newly-captured `principal_kind`
  (`user`/`apikey`/`service`) and `on_behalf_of` (api-key only). **Absent `suspended` is treated as
  *unknown → deny*, never `false`.** A verified contract is not cached past its `exp`.
- **A valid contract satisfies the acting-scope requirement** in the sub-mode, **replacing** the
  `x-tenant-scope: acting` tripwire (which stays intact when the sub-mode is off).
- The `plan` default header aligns to `x-workspace-plan`; the retired bare role/entitlement/
  suspended headers are no longer the source of truth when a contract is present.
- **BREAKING (config-gated, opt-in only):** enabling `trusted.contract` changes where sensitive
  identity comes from and drops the scope-header gate for that path. Default (sub-mode off) behavior
  is unchanged.
- A boot guard requires `jwks_url`, `issuer`, and `audience` to be set whenever the sub-mode is
  enabled; `docs/design/multitenant-trust.md` is updated (its header-reading section is stale).

## Capabilities

### New Capabilities

- `identity-contract-verification`: verifying nexus's signed `x-identity-contract` (ES256 JWS) —
  JWKS acquisition/caching/rotation, signature verification without introducing a second crypto
  stack, registered-claim checks (`iss`/`aud`/`exp`), the `ctr` version drift gate, fail-closed
  behavior on an enriched route, and the freshness bound (no caching past `exp`).

### Modified Capabilities

- `tenant-identity`: in the contract sub-mode, sensitive identity (roles, entitlements, suspended,
  plan) and the newly-captured `principal_kind`/`on_behalf_of` are sourced from the **verified
  claims** rather than bare headers; absent `suspended` is *unknown → deny*; a verified contract
  **replaces** the `x-tenant-scope: acting` gate; the generic header-trust path is preserved and
  unchanged when the sub-mode is off.

## Impact

- **Code:** `crates/runlet/src/identity.rs` (claim-sourced identity + `principal_kind`), the gates in
  `crates/runlet/src/handler/gates.rs` (acting-scope gate becomes contract-satisfied in the
  sub-mode), `crates/runlet/src/config/mod.rs` + `config/guards.rs` (new `trusted.contract` config +
  boot guard), `quota.rs` (plan header alignment), and a new contract-verification module.
- **Dependencies:** a JWS/ES256 **verify** path and a JWKS fetch/cache client. Both are
  correctness-/security-critical and constrained by the box's hard **aws-lc-rs-only, `ring`-free**
  invariant (`cargo tree -i ring` must stay empty) — the concrete crate/approach is deferred to the
  build-vs-adopt gate (`/opsx:decide`) and recorded in `design.md`. JWKS fetch adds outbound HTTP,
  new ambient authority for a box that otherwise holds no creds and links no driver.
- **Cross-repo (out of scope, noted as follow-ups):** event-logs tenant isolation + the
  `X-Runlet-Tenant` ingest route, and the missing canonical cross-repo vocabulary artifact
  (`Nexus-IDS.md` / `nexus-upstream-requirements.md` that nexus references but never wrote).
- **Docs:** `docs/design/multitenant-trust.md` refreshed; box-consumer expectations documented.
