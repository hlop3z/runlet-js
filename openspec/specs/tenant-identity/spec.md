# tenant-identity Specification

## Purpose

The trusted-header identity contract for running `/execute` behind the nexus edge: the box derives
tenant + user identity solely from operator-configured trusted headers the edge injects, rejects
anonymous/suspended principals, is protected by a trusted-headers boot guard (network-isolation
assertion), and can gate capabilities on a member's roles/entitlements. Rationale:
`docs/design/multitenant-trust.md`.

## Requirements

### Requirement: Trusted identity ingress

When running in trusted-header mode, the system SHALL derive the request's tenant and user
identity solely from operator-configured trusted headers injected by the edge, and SHALL
ignore any identity value supplied by the client. The tenant identifier (`x-workspace-id` by
default, matching the header the nexus identity sidecar injects) is treated as an opaque,
already-authorized acting-workspace id.

#### Scenario: Tenant identity is taken from the trusted header

- **WHEN** a request arrives with the configured tenant header set to a workspace id
- **THEN** that value is used as the request's tenant identity and echoed in `meta`

#### Scenario: Client-supplied identity is ignored

- **WHEN** a request carries both a client-set identity value and the trusted headers
- **THEN** only the trusted-header values are used and the client-set value has no effect

#### Scenario: Missing tenant identity for tenant-scoped work

- **WHEN** trusted-header mode is enabled and a request requires tenant scope but carries no tenant header
- **THEN** the request is rejected and no execution or egress session begins

### Requirement: Identity from the verified contract

The system SHALL, when the contract sub-mode is enabled and a request carries a verified
`x-identity-contract`, source the request's identity from the **verified claims** as the
authoritative values: the tenant from `workspace_id`, the user from `sub`, the plan from `plan`, the
caller's roles from `roles`, entitlements from `entitlements`, the newly-captured principal kind from
`principal_kind` (`user`/`apikey`/`service`), and — for an api-key principal only — the acted-for
subject from `on_behalf_of`. Non-sensitive plain headers MAY still be read, but the verified claims
SHALL take precedence when a contract is present. An absent `plan` claim SHALL be treated as
not-provisioned (no tier granted), never defaulted.

#### Scenario: Sensitive identity comes from claims, not bare headers

- **WHEN** a verified contract is present and the retired bare role/entitlement/suspended headers are absent
- **THEN** roles, entitlements, and suspension are taken from the contract's claims and the request proceeds on those values

#### Scenario: Principal kind is captured

- **WHEN** a verified contract carries `principal_kind`
- **THEN** the request's principal kind is recorded from that claim (and `on_behalf_of` when the kind is api-key)

#### Scenario: Absent plan is not-provisioned

- **WHEN** a verified contract omits the `plan` claim
- **THEN** the tenant is treated as not-provisioned for plan-gated decisions rather than assigned a default tier

### Requirement: Reject anonymous and suspended principals

The system SHALL reject a request whose trusted identity indicates an anonymous caller or a
suspended principal, because executing caller-supplied code requires an authenticated, active
principal. In the contract sub-mode, suspension SHALL be read from the verified contract's
`suspended` claim, and an **absent `suspended` claim SHALL be treated as unknown and fail safe
(reject)** — never as not-suspended. In the plain trusted-header path, anonymity and suspension are
read from their configured headers as before.

#### Scenario: Anonymous request refused

- **WHEN** a request's trusted identity indicates an anonymous caller
- **THEN** the response is an authorization failure and no handler runs

#### Scenario: Suspended principal refused

- **WHEN** a verified contract's `suspended` claim is true (or, in the plain path, the suspended header is true)
- **THEN** the response is an authorization failure and no handler runs

#### Scenario: Unknown suspension fails safe

- **WHEN** the contract sub-mode is enabled and a verified contract omits the `suspended` claim
- **THEN** the request is rejected with an authorization failure (absence is unknown, not not-suspended)

### Requirement: Acting-org assurance is mandatory in trusted mode

When running in trusted-header mode, the system SHALL require per-request assurance that the
request's tenant identity is the caller's **authorized acting org** before any egress session or
execution begins, and SHALL reject a tenant-scoped request lacking that assurance with an
authorization failure. The system SHALL continue to treat the tenant identifier as opaque — it never
derives the org relationship itself. The assurance MAY be satisfied two ways depending on
configuration:

- **In the contract sub-mode**, a **verified `x-identity-contract`** (valid signature, `iss`, `aud`,
  `exp`, and supported `ctr`) IS the acting-org assurance — the contract is minted by nexus only for
  a caller resolved to an authorized acting workspace — and the separate scope-header tripwire SHALL
  NOT be additionally required.
- **In the plain trusted-header path** (sub-mode off), the assurance is the configurable trusted
  scope header (`x-tenant-scope` by default) with value `acting`; a tenant-scoped request whose
  scope header is absent or not equal to `acting` SHALL be rejected with `403 ACTING_SCOPE_REQUIRED`.
  The scope header value is client-unspoofable on the same basis as every other trusted header; the
  guard is a fail-closed contract tripwire, not cryptographic proof.

#### Scenario: Verified contract satisfies acting-org assurance

- **WHEN** the contract sub-mode is enabled and a tenant-scoped request carries a fully verified contract
- **THEN** the acting-org assurance is satisfied and the scope header is not additionally required

#### Scenario: Plain path still requires the acting scope header

- **WHEN** the contract sub-mode is disabled and a tenant-scoped request carries a tenant header but no scope header equal to `acting`
- **THEN** the request is rejected with `403 ACTING_SCOPE_REQUIRED` and no execution or egress session begins

#### Scenario: Non-acting scope is refused in the plain path

- **WHEN** the contract sub-mode is disabled and a tenant-scoped request carries a scope header whose value is not `acting`
- **THEN** the request is rejected with an authorization failure and no execution or egress session begins

#### Scenario: Non-trusted mode is unaffected

- **WHEN** trusted-header mode is disabled (single-tenant / loopback)
- **THEN** neither the scope header nor the contract is consulted and request handling is unchanged

### Requirement: Trusted-headers boot guard

The system SHALL refuse to start in trusted-header mode when bound to a non-loopback address
unless the operator has explicitly asserted network isolation, so identity headers are never
trusted on an exposed bind. When configured, the edge service credential SHALL be required on
inbound requests as defense in depth.

#### Scenario: Exposed bind without asserted isolation refuses to start

- **WHEN** trusted-header mode is enabled, the bind address is non-loopback, and isolation is not asserted
- **THEN** the process refuses to start with a configuration error

#### Scenario: Missing service credential is rejected

- **WHEN** the edge service credential is configured and an inbound request omits or mismatches it
- **THEN** the request is rejected before identity is trusted

### Requirement: Coarse member capability authorization

The system SHALL support gating a requested capability against the caller's trusted roles or
entitlements, so a member without the required role/entitlement cannot invoke that capability even
within their own workspace. In the contract sub-mode, the caller's roles and entitlements SHALL be
taken from the verified contract's `roles`/`entitlements` claims; in the plain trusted-header path
they are taken from their configured headers.

#### Scenario: Member lacks the required entitlement

- **WHEN** a request invokes a capability the caller's roles/entitlements (from the verified claims, or from the configured headers in the plain path) do not permit
- **THEN** the request is rejected before the capability runs

#### Scenario: Member holds the required entitlement

- **WHEN** the caller's roles/entitlements permit the requested capability
- **THEN** the capability proceeds subject to the remaining tenant checks

### Requirement: Box-wide principal-kind admission

The system SHALL support restricting a box to a configured set of principal kinds via a box-wide
allowlist (`trusted.allowed_principal_kinds`), so an operator can dedicate an instance to one class of
principal (a user-box, a service-box) that scales and is routed independently. When the allowlist is
non-empty, the system SHALL admit a request only if its verified `principal_kind` is a member of the
allowlist, and SHALL otherwise reject it with `403 PRINCIPAL_KIND_FORBIDDEN`. The decision SHALL be a
pure function of `principal_kind` — `on_behalf_of` SHALL NOT promote an api-key principal to a human for
this gate. An **empty allowlist SHALL admit every kind** (the default, preserving prior behavior). The
check SHALL run at caller admission (alongside the anonymous / suspended / tenant-required checks),
evaluated once per request and once per batch, and SHALL NOT be evaluated on the per-capability path
(the coarse member-capability gate is unaffected).

#### Scenario: Kind in the allowlist is admitted

- **WHEN** `allowed_principal_kinds` is `["service"]` and a request carries a verified contract whose `principal_kind` is `service`
- **THEN** the principal-kind gate admits the request and execution proceeds to the remaining gates

#### Scenario: Kind absent from the allowlist is rejected

- **WHEN** `allowed_principal_kinds` is `["service"]` and a request carries a verified contract whose `principal_kind` is `user`
- **THEN** the system rejects the request with `403 PRINCIPAL_KIND_FORBIDDEN` and records an audit denial

#### Scenario: Absent principal kind fails closed

- **WHEN** `allowed_principal_kinds` is non-empty and a request's `principal_kind` is absent (no verified contract populated it)
- **THEN** the system rejects the request with `403 PRINCIPAL_KIND_FORBIDDEN`, because an absent kind is never a member of a non-empty allowlist

#### Scenario: Empty allowlist admits every kind

- **WHEN** `allowed_principal_kinds` is empty (the default)
- **THEN** the principal-kind gate admits the request regardless of its `principal_kind`, including an absent kind

#### Scenario: on_behalf_of does not promote an api-key across the gate

- **WHEN** `allowed_principal_kinds` is `["user"]` and a request carries a verified contract whose `principal_kind` is `apikey` with an `on_behalf_of` human subject present
- **THEN** the system rejects the request with `403 PRINCIPAL_KIND_FORBIDDEN`, because the gate decides on `principal_kind` alone

#### Scenario: A configured allowlist without the contract sub-mode refuses to start

- **WHEN** `allowed_principal_kinds` is non-empty but `trusted.contract.enabled` is false
- **THEN** the box refuses to start with a boot-guard error, because `principal_kind` is only ever populated by a verified contract and the box would otherwise deny every request
