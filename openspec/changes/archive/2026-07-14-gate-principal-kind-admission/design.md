## Context

`principal_kind` (`user`/`apikey`/`service`) is extracted from the verified signed identity contract
(`identity.rs::apply_contract`) and forwarded to egress (broker `WireInit` + box-direct headers), but
no code gates on it. The trusted-identity admission path in `gates.rs` already rejects anonymous,
suspended, and tenantless callers before any capability runs; the coarse member gate (`authz.rs`,
`enforce_member_authz`) separately checks `capability → entitlement`. `docs/design/multitenant-trust.md`
records kind-gating as an explicitly deferred concern.

A design conversation settled the model: rather than a per-capability kind allowlist, the operator
deploys a **dedicated box per principal kind** (user-box, service-box) and each box declares which kinds
it serves. The system is small, so running 2–3 instances is cheap, and separate instances give stronger
isolation (a user-box is never even configured with service traffic) plus independent scaling. The gate
therefore reduces to a single box-wide admission check.

Constraint: `principal_kind` is populated **only** by a verified contract. In plain trusted-header mode
it is always `None`. Any gate on kind is only meaningful under `trusted.contract.enabled`.

## Goals / Non-Goals

**Goals:**

- A box-wide allowlist that admits a request only when its verified `principal_kind` is a member.
- Fail-closed: an absent kind never passes a non-empty allowlist.
- A boot guard that refuses to start when the allowlist is configured without the contract sub-mode.
- Zero change to the capability path, `authz.rs`, `runlet-core`, and the wire contract.
- Backward compatible: default-empty allowlist admits everything.

**Non-Goals:**

- Per-capability kind policy (a `capability → allowed-kinds` map) — explicitly rejected in favor of the
  box-wide model.
- Per-script kind requirements — a future, separate axis (script metadata, not operator admission).
- `on_behalf_of`-based promotion of an api-key to a human — the gate is a pure function of `principal_kind`.
- Any change to how nexus routes or mints tokens — this is a box-local decision.

## Decisions

### Build vs. adopt

**Build (trivially).** The gate is one set-membership check over an in-memory `Vec<String>` against a
single `Option<String>` field already present on `TrustedIdentity`. There is nothing to adopt — no
policy engine, no external dependency. A general policy engine (e.g. Cedar) is the documented v2 path
for fine-grained role→resource authz and is out of scope here.

### Box-wide allowlist over per-capability grain

Chosen because the deployment model is "one box per kind," which the per-capability map cannot improve
on for this system and which yields stronger isolation than any in-box gate: a user-box literally has no
service capabilities configured. The allowlist is defense-in-depth on top of the contract `aud` claim
(nexus scopes each token to one box pool, so a mis-routed token already fails the audience check) — a
fourth independent control in the same spirit as the existing NetworkPolicy + boot guard + edge
credential trio.

### The gate lives at caller admission, evaluated once

`principal_kind` is a property of the caller/contract, not of a capability or a batch item, so the check
belongs beside the anonymous / suspended / tenant-required checks in the identity admission path, not on
the per-capability path. It is evaluated once per `/execute` and once per `/batch` (batch resolves
identity at batch level). This keeps `authorize_capabilities` a pure `capability → entitlement`
set-membership function and adds no per-item cost. Alternative considered: extend
`authorize_capabilities` with a second map — rejected because it tangles two orthogonal axes (*what you
are* vs *what you hold*) and would run the kind check redundantly per capability.

### Fail-closed on an absent kind

A non-empty allowlist never contains `None`, so an absent `principal_kind` is denied. This mirrors the
existing `SUSPENDED_UNKNOWN → deny` precedent and prevents a caller from bypassing the gate by dropping
the contract. Enforced structurally: the check is "is the request's kind a member," and `None` matches
no member.

### Boot guard: a configured allowlist requires the contract sub-mode

Because an absent kind always denies and the plain-header path never populates kind, a non-empty
allowlist without `trusted.contract.enabled` would deny 100% of requests — a silent, total outage. The
box refuses to start on that combination, surfacing the misconfiguration at boot rather than as a
mysterious universal 403. This mirrors `check_contract_config`, which already refuses to start when the
contract is enabled but its required fields are unset.

### Denial shape

A dedicated `403 PRINCIPAL_KIND_FORBIDDEN` (distinct from `ENTITLEMENT_REQUIRED`), routed through the
same `emit_denied` audit path the other admission gates use, so a denial is observable in the audit
stream with its own reason code.

## Risks / Trade-offs

- **Operator enables the allowlist without the contract sub-mode** → the boot guard refuses to start, so
  the failure is loud and immediate rather than a silent all-deny at request time.
- **Kind name typo in config** (e.g. `"services"`) → that box admits nothing of the intended kind and
  fails closed. Mitigation: validate configured names against the known kind set (`user`/`apikey`/`service`)
  at boot and refuse to start on an unknown value, so a typo is caught like the missing-contract case
  rather than surfacing as runtime denials.
- **Reduced flexibility vs. per-capability policy** → accepted deliberately; the deployment-topology
  model is the intended design, and the per-capability map remains a documented future option if a
  mixed-kind box is ever needed.
- **Divergence from the `aud` control** → none; the two are complementary and independent. If nexus
  routing changes, the box-local allowlist still holds.

## Migration Plan

Additive and backward compatible. Existing deployments set no `allowed_principal_kinds`, so the field
defaults empty and every kind is admitted — no behavior change. To adopt: enable the contract sub-mode
(already required for trusted identity), then set `allowed_principal_kinds` on each box's config and
route the matching traffic to it. Rollback is removing the field (or emptying it), which reopens the box
to all kinds. No data migration, no wire change, no coordinated broker/edge change.

## Open Questions

- ~~Should the box validate configured kind names against the closed set `{user, apikey, service}` at
  boot and refuse to start on an unknown value?~~ **Resolved: yes.** The boot guard rejects an
  `allowed_principal_kinds` entry outside `{user, apikey, service}`, turning a typo (e.g. `"services"`)
  into an immediate boot error rather than silent runtime all-deny — consistent with the fail-fast posture.
