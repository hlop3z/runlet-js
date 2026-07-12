## ADDED Requirements

### Requirement: Tenant identity carried on the box-direct local egress path

The box SHALL convey the request's trusted tenant identity to a box-direct loopback endpoint (an
operator-declared co-located resource) as an out-of-band HTTP header (`X-Runlet-Tenant`), and SHALL do
so when, and only when, a trusted tenant identity is present for the request. This mirrors the broker
path, where the tenant rides the session handshake (`WireInit.tenant`) rather than the per-call body.

The tenant identity SHALL be sourced only from the trusted-identity extractor and SHALL NEVER be
sourced from a value the executing script can influence. When no trusted tenant identity is present
(the single-tenant / non-trusted path), the box SHALL add no such header and the box-direct request
SHALL be unchanged.

The header is out of band: the box-direct request **body** SHALL remain the identical
`{action, payload}` envelope a broker receives, so a service can still be moved between box-direct and
broker resolution with no change to the calling script or the wire body. This requirement applies to
the box-direct path only; the `http` and `s3` built-in capabilities SHALL carry no tenant identity.

#### Scenario: Box-direct call in a tenant context carries the tenant header

- **WHEN** trusted-header mode is enabled, a request resolves to a trusted tenant `"ws_acme"`, and the
  script calls `$std.io.call("pricing", action, payload)` for a box-direct-bound name `"pricing"`
- **THEN** the box POSTs `{action, payload}` to the local endpoint with an `X-Runlet-Tenant: ws_acme`
  header, and the request body is unchanged from the no-tenant case

#### Scenario: No trusted tenant, no header

- **WHEN** a box-direct call is made on the single-tenant / non-trusted path (no trusted tenant
  identity is present)
- **THEN** the box-direct POST carries no `X-Runlet-Tenant` header and is byte-for-byte the same as
  before this change

#### Scenario: Script cannot influence the tenant header

- **WHEN** the executing script or the request body attempts to set or override a tenant value
- **THEN** the `X-Runlet-Tenant` header reflects only the trusted-identity extractor's tenant (or is
  absent), never a script- or caller-asserted value

#### Scenario: http and s3 carry no tenant

- **WHEN** a request uses the `http` or `s3` built-in capability while a trusted tenant identity is
  present
- **THEN** no tenant identity is attached to those calls (they target script-controlled or
  externally-signed endpoints, not the operator's privatized resources)
