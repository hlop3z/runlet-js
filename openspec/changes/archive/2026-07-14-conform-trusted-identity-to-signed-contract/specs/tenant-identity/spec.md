## ADDED Requirements

### Requirement: Identity from the verified contract

The system SHALL, when the contract sub-mode is enabled and a request carries a verified
`x-identity-contract`, source the request's identity from the **verified claims** as the
authoritative values:
the tenant from `workspace_id`, the user from `sub`, the plan from `plan`, the caller's roles from
`roles`, entitlements from `entitlements`, the newly-captured principal kind from `principal_kind`
(`user`/`apikey`/`service`), and — for an api-key principal only — the acted-for subject from
`on_behalf_of`. Non-sensitive plain headers MAY still be read, but the verified claims SHALL take
precedence when a contract is present. An absent `plan` claim SHALL be treated as not-provisioned
(no tier granted), never defaulted.

#### Scenario: Sensitive identity comes from claims, not bare headers

- **WHEN** a verified contract is present and the retired bare role/entitlement/suspended headers are absent
- **THEN** roles, entitlements, and suspension are taken from the contract's claims and the request proceeds on those values

#### Scenario: Principal kind is captured

- **WHEN** a verified contract carries `principal_kind`
- **THEN** the request's principal kind is recorded from that claim (and `on_behalf_of` when the kind is api-key)

#### Scenario: Absent plan is not-provisioned

- **WHEN** a verified contract omits the `plan` claim
- **THEN** the tenant is treated as not-provisioned for plan-gated decisions rather than assigned a default tier

## MODIFIED Requirements

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
