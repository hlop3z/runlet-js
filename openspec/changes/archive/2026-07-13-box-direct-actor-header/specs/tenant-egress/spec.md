## ADDED Requirements

### Requirement: Actor identity carried on the box-direct local egress path

The box SHALL convey the request's trusted acting **subject** to a box-direct loopback endpoint (an
operator-declared co-located resource) as an out-of-band HTTP header (`X-Runlet-Actor`), and SHALL do so
when, and only when, a trusted subject is present for the request. The header value SHALL be the bare
subject; the box SHALL NOT encode principal kind, roles, entitlements, or any other identity field into
it. This companions the existing `X-Runlet-Tenant` requirement: the tenant answers *where* the action
happens, the actor answers *who* is acting.

The subject SHALL be sourced only from the trusted-identity extractor (the acting-user field) and SHALL
NEVER be sourced from a value the executing script or caller can influence — an actor identity is a trust
assertion and therefore MUST NOT be read from the script `payload` (unlike a routing/stream key, which
may). When no trusted subject is present (the single-tenant / non-trusted path), the box SHALL add no
such header and the box-direct request SHALL be unchanged.

The header is out of band: the box-direct request **body** SHALL remain the identical `{action, payload}`
envelope a broker receives, so a service can still be moved between box-direct and broker resolution with
no change to the calling script or the wire body. This requirement applies to the box-direct path only;
the `http` and `s3` built-in capabilities SHALL carry no actor identity, and the broker path SHALL be
unchanged.

Principal **kind** (user / apikey / service) SHALL NOT be forwarded by this requirement. Kind is available
to the box only inside a signed identity assertion, and the box does not verify signed assertions; should
a consumer later require kind, it SHALL be conveyed as a separate trusted header so that `X-Runlet-Actor`
remains stably equal to the bare subject.

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
- **THEN** the box-direct POST carries no `X-Runlet-Actor` header and is byte-for-byte the same as before
  this change with respect to the actor

#### Scenario: Script cannot influence the actor header

- **WHEN** the executing script or the request `payload` attempts to set or override an actor value
- **THEN** the `X-Runlet-Actor` header reflects only the trusted-identity extractor's subject (or is
  absent), never a script- or caller-asserted value

#### Scenario: http and s3 carry no actor

- **WHEN** a request uses the `http` or `s3` built-in capability while a trusted subject is present
- **THEN** no actor identity is attached to those calls (they target script-controlled or
  externally-signed endpoints, not the operator's privatized resources)
