## MODIFIED Requirements

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
path) the handshake SHALL carry no actor.

When a **verified identity contract** populated the request's principal kind and/or its acted-for subject,
the box SHALL ALSO carry those as **separate** `WireInit` fields — `principal_kind` (`user`/`apikey`/
`service`) and `on_behalf_of` (the creating human for an api-key) — alongside `actor`. Each SHALL be
included when, and only when, present, and SHALL be sourced only from the verified contract via the
trusted-identity extractor. `actor` SHALL remain exactly the bare acting subject (the key id for an api-key
principal); the kind and on-behalf-of ride beside it, so a consumer can attribute the action to both the
acting subject and the human, and branch on the kind. On the plain trusted-header path and the
single-tenant/loopback path (no verified contract), neither field SHALL be present and the handshake SHALL
be unchanged from before this requirement.

#### Scenario: Session opens with the trusted acting subject

- **WHEN** a request that names broker-resolved egress resources opens a broker session and a trusted subject
  `"u_42"` is present
- **THEN** the handshake carries `"u_42"` as the acting subject alongside the `resources` list and the trusted
  tenant

#### Scenario: No trusted subject, no actor on the session

- **WHEN** a broker session opens on the single-tenant/loopback path with no trusted subject present
- **THEN** the handshake carries no acting subject, and no principal kind or on-behalf-of

#### Scenario: Script cannot influence the session actor

- **WHEN** the executing script or the request payload attempts to set or override an actor value
- **THEN** the handshake's acting subject reflects only the trusted-identity extractor's subject (or is
  absent), never a script- or caller-asserted value

#### Scenario: Actor forwarding is transport-independent

- **WHEN** the same logical `io` name is served box-direct in one deployment and via a QUIC broker in another,
  and a trusted subject `"u_42"` is present
- **THEN** the co-located target receives `"u_42"` via the `X-Runlet-Actor` header and the remote target
  receives `"u_42"` via `WireInit`, so moving the service between transports does not drop the actor

#### Scenario: Verified api-key contract carries kind and on-behalf-of on the session

- **WHEN** a broker session opens for a request whose verified contract has `principal_kind = "apikey"`,
  `actor` (sub) the key id `"key_9"`, and `on_behalf_of` the human `"u_42"`
- **THEN** the handshake carries `actor = "key_9"`, `principal_kind = "apikey"`, and `on_behalf_of = "u_42"`
  as separate fields, so a consumer attributes the action to both the key and the human

#### Scenario: Plain-path session carries no kind or on-behalf-of

- **WHEN** a broker session opens on the plain trusted-header path (no verified contract), with a trusted
  subject present
- **THEN** the handshake carries the acting subject but no `principal_kind` and no `on_behalf_of`

### Requirement: Actor identity carried on the box-direct local egress path

The box SHALL convey the request's trusted acting **subject** to a box-direct loopback endpoint (an
operator-declared co-located resource) as an out-of-band HTTP header (`X-Runlet-Actor`), and SHALL do so
when, and only when, a trusted subject is present for the request. The `X-Runlet-Actor` value SHALL be the
bare subject and SHALL NOT encode principal kind, roles, entitlements, or any other field. This companions
the existing `X-Runlet-Tenant` requirement: the tenant answers *where* the action happens, the actor answers
*who* is acting.

The subject SHALL be sourced only from the trusted-identity extractor (the acting-user field) and SHALL
NEVER be sourced from a value the executing script or caller can influence — an actor identity is a trust
assertion and therefore MUST NOT be read from the script `payload` (unlike a routing/stream key, which
may). When no trusted subject is present (the single-tenant / non-trusted path), the box SHALL add no
`X-Runlet-Actor` header.

When a **verified identity contract** populated the request's principal kind and/or its acted-for subject,
the box SHALL ALSO convey those as **separate** out-of-band headers — `X-Runlet-Principal-Kind`
(`user`/`apikey`/`service`) and `X-Runlet-On-Behalf-Of` (the creating human for an api-key) — attached when,
and only when, present, and sourced only from the verified contract. `X-Runlet-Actor` SHALL remain the bare
subject; the new headers ride beside it. On the plain trusted-header path and the single-tenant path (no
verified contract), neither header SHALL be present.

The headers are out of band: the box-direct request **body** SHALL remain the identical `{action, payload}`
envelope a broker receives, so a service can still be moved between box-direct and broker resolution with
no change to the calling script or the wire body. This requirement applies to the box-direct path only;
the `http` and `s3` built-in capabilities SHALL carry no actor identity.

#### Scenario: Box-direct call with a trusted subject carries the actor header

- **WHEN** trusted-header mode is enabled, a request resolves to a trusted subject `"u_42"`, and the
  script calls `$std.io.call("pricing", action, payload)` for a box-direct-bound name `"pricing"`
- **THEN** the box POSTs `{action, payload}` to the local endpoint with an `X-Runlet-Actor: u_42` header,
  and the request body is unchanged from the no-actor case

#### Scenario: Actor and tenant headers ride together

- **WHEN** a box-direct call is made with both a trusted tenant `"ws_acme"` and a trusted subject `"u_42"`
- **THEN** the POST carries both `X-Runlet-Tenant: ws_acme` and `X-Runlet-Actor: u_42`, and the body is
  still exactly `{action, payload}`

#### Scenario: No trusted subject, no actor header

- **WHEN** a box-direct call is made on the single-tenant / non-trusted path (no trusted subject is
  present), even when a trusted tenant is present
- **THEN** the box-direct POST carries no `X-Runlet-Actor`, `X-Runlet-Principal-Kind`, or
  `X-Runlet-On-Behalf-Of` header

#### Scenario: Script cannot influence the actor header

- **WHEN** the executing script or the request `payload` attempts to set or override an actor value
- **THEN** the `X-Runlet-Actor` header reflects only the trusted-identity extractor's subject (or is
  absent), never a script- or caller-asserted value

#### Scenario: http and s3 carry no actor

- **WHEN** a request uses the `http` or `s3` built-in capability while a trusted subject is present
- **THEN** no actor identity is attached to those calls (they target script-controlled or
  externally-signed endpoints, not the operator's privatized resources)

#### Scenario: Verified api-key contract carries kind and on-behalf-of headers box-direct

- **WHEN** a box-direct call is made for a request whose verified contract has `principal_kind = "apikey"`,
  subject the key id `"key_9"`, and `on_behalf_of` the human `"u_42"`
- **THEN** the POST carries `X-Runlet-Actor: key_9`, `X-Runlet-Principal-Kind: apikey`, and
  `X-Runlet-On-Behalf-Of: u_42`, and the body is still exactly `{action, payload}`

#### Scenario: Plain-path box-direct call carries no kind or on-behalf-of header

- **WHEN** a box-direct call is made on the plain trusted-header path (no verified contract) with a trusted
  subject present
- **THEN** the POST carries `X-Runlet-Actor` but neither `X-Runlet-Principal-Kind` nor `X-Runlet-On-Behalf-Of`
