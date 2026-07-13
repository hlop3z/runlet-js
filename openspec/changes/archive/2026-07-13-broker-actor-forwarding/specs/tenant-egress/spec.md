## ADDED Requirements

### Requirement: Actor identity carried on the egress session

The box SHALL include the request's trusted acting **subject** in the broker session handshake
(`WireInit`) when opening an egress session, so a broker (and any backend it resolves to) can attribute
who-did-what regardless of transport. This companions the "Tenant identity carried on the egress session"
requirement: the tenant scopes *where* resolution happens, the actor records *who* is acting. It also makes
actor forwarding transport-independent — a consumer reached box-direct receives the acting subject as the
`X-Runlet-Actor` header, and a consumer reached over a UDS or QUIC broker receives the same subject in
`WireInit`.

The subject SHALL be the bare acting-user identity and SHALL be sourced only from the trusted-identity
extractor; it SHALL NEVER be sourced from a value the executing script or caller can influence. It SHALL be
included when, and only when, a trusted subject is present; when none is present (the single-tenant/loopback
path) the handshake SHALL carry no actor and SHALL be unchanged from before this requirement.

The addition SHALL be backward-compatible on the wire: a broker that does not read the actor SHALL be
unaffected (the field is optional and absent when no subject is present). Principal **kind** (user / apikey /
service) SHALL NOT be included by this requirement — it is crypto-gated and, if ever required, arrives as a
separate field, leaving the actor equal to the bare subject.

#### Scenario: Session opens with the trusted acting subject

- **WHEN** a request that names broker-resolved egress resources opens a broker session and a trusted subject
  `"u_42"` is present
- **THEN** the handshake carries `"u_42"` as the acting subject alongside the `resources` list and the trusted
  tenant

#### Scenario: No trusted subject, no actor on the session

- **WHEN** a broker session opens on the single-tenant/loopback path with no trusted subject present
- **THEN** the handshake carries no acting subject and is unchanged from before this requirement

#### Scenario: Script cannot influence the session actor

- **WHEN** the executing script or the request payload attempts to set or override an actor value
- **THEN** the handshake's acting subject reflects only the trusted-identity extractor's subject (or is
  absent), never a script- or caller-asserted value

#### Scenario: Actor forwarding is transport-independent

- **WHEN** the same logical `io` name is served box-direct in one deployment and via a QUIC broker in another,
  and a trusted subject `"u_42"` is present
- **THEN** the co-located target receives `"u_42"` via the `X-Runlet-Actor` header and the remote target
  receives `"u_42"` via `WireInit`, so moving the service between transports does not drop the actor
