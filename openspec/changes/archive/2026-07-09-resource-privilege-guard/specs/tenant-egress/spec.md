## MODIFIED Requirements

### Requirement: Tenant identity carried on the egress session

The box SHALL include the request's trusted tenant identity in the `fabricd` session
handshake (`WireInit`) when opening an egress session, so the daemon can scope resolution to
that tenant. The tenant identity SHALL never be sourced from a value the executing script can
influence. The **presence** of a trusted tenant identity on the session SHALL itself mark the
session as the untrusted-tenant (multitenant/nexus) context: it is the trigger for the
least-privilege mandate below. The box SHALL NOT add any separate privilege signal to the
handshake — the wire contract is unchanged, and the daemon derives the mandate from the tenant
identity it already receives.

#### Scenario: Session opens with the trusted tenant id

- **WHEN** a request that names driver resources opens a `fabricd` session
- **THEN** the handshake carries the request's trusted tenant identity

#### Scenario: No tenant identity, no tenant-scoped session

- **WHEN** trusted-header mode is enabled and no trusted tenant identity is present
- **THEN** no tenant-scoped egress session is opened

#### Scenario: A tenant-scoped session marks the multitenant context

- **WHEN** a session opens carrying a trusted tenant identity
- **THEN** that session is the multitenant context for the least-privilege mandate, derived by
  the daemon from the tenant identity alone (no additional handshake field)

#### Scenario: Single-tenant session is not a multitenant context

- **WHEN** a session opens on the single-tenant/loopback path with no trusted tenant identity
- **THEN** the session is not a multitenant context and the handshake is byte-identical to a
  session that predates this change (the wire contract is unchanged)

## ADDED Requirements

### Requirement: Multitenant path forbids the privilege opt-out

When a session carries a trusted tenant identity (the multitenant context), `fabricd` SHALL
treat any per-resource privilege opt-out (`allow_privileged`) as void for that session and SHALL
refuse to serve a resource flagged over-privileged by the privilege preflight, regardless of that
resource's configuration. The opt-out exists only for a trusted solo operator accepting the risk
for their own scripts; it SHALL never weaken a deployment that serves untrusted tenants. `fabricd`
SHALL derive the multitenant context from the tenant identity already present in the handshake — no
separate least-privilege signal is carried on the wire.

#### Scenario: Flagged resource with opt-out is still refused on the multitenant path

- **WHEN** a session carries a trusted tenant identity and requests a resource flagged
  over-privileged that has `allow_privileged: true` set
- **THEN** `fabricd` refuses to serve that resource for the session

#### Scenario: Opt-out still applies on the single-tenant path

- **WHEN** a session carries no trusted tenant identity and requests a resource flagged
  over-privileged that has `allow_privileged: true` set
- **THEN** `fabricd` serves that resource
