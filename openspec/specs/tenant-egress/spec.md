# tenant-egress Specification

## Purpose

Tenant-scoped egress isolation: the box forwards the request's trusted tenant identity in the
box↔broker session handshake, and the broker resolves logical resource names only within that
tenant's authorized binding set — so credentials and resources never cross workspace boundaries.
Rationale: `docs/design/multitenant-trust.md`, `docs/design/resource-egress.md`.
## Requirements
### Requirement: Logical resource names on the egress session

Egress SHALL address a **flat list of logical resource names**. The request SHALL declare the names
it may touch as a plain allowlist (`config.io: ["orders", "cache"]`); the box SHALL forward the
selected names to the broker in the session handshake as `resources` (a list of names), never as
per-kind fields. The resource *kind* and *transport* SHALL be resolved entirely by the broker
(operator-side); the box SHALL remain kind-blind and hold no backend endpoint or credential (Model 1:
the box's only egress peer is the broker, reached over UDS locally or QUIC remotely). A name absent
from `config.io` SHALL be rejected.

#### Scenario: Script addresses a logical name only

- **WHEN** a script calls `$std.io.call("orders", "query", payload)` and `"orders"` is in `config.io`
- **THEN** the box forwards the name `"orders"` to the broker, which resolves its kind/endpoint/creds

#### Scenario: Unlisted name is rejected

- **WHEN** a script calls `$std.io.call("secret", …)` and `"secret"` is not in `config.io`
- **THEN** the call is rejected (`RESOURCE_NOT_FOUND`) before any egress

#### Scenario: Handshake carries a flat name list

- **WHEN** a request that names egress resources opens a broker session
- **THEN** the handshake carries `resources` as a flat list of logical names (no per-kind slots)

### Requirement: Tenant identity carried on the egress session

The box SHALL include the request's trusted tenant identity in the broker session
handshake (`WireInit`) when opening an egress session, so the broker can scope resolution to
that tenant. The tenant identity SHALL never be sourced from a value the executing script can
influence. The **presence** of a trusted tenant identity on the session SHALL itself mark the
session as the untrusted-tenant (multitenant/nexus) context — the trigger for the least-privilege
mandate below. The box SHALL NOT add any separate privilege signal to the handshake: the wire
contract is unchanged, and the broker derives the mandate from the tenant identity it already
receives.

#### Scenario: Session opens with the trusted tenant id

- **WHEN** a request that names egress resources opens a broker session
- **THEN** the handshake carries the request's trusted tenant identity alongside the `resources` list

#### Scenario: No tenant identity, no tenant-scoped session

- **WHEN** trusted-header mode is enabled and no trusted tenant identity is present
- **THEN** no tenant-scoped egress session is opened

#### Scenario: A tenant-scoped session marks the multitenant context

- **WHEN** a session opens carrying a trusted tenant identity
- **THEN** that session is the multitenant context for the least-privilege mandate, derived by
  the broker from the tenant identity alone (no additional handshake field)

#### Scenario: Single-tenant session is not a multitenant context

- **WHEN** a session opens on the single-tenant/loopback path with no trusted tenant identity
- **THEN** the session is not a multitenant context and the handshake carries no separate
  privilege signal (the wire contract is unchanged)

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

### Requirement: Tenant-scoped resource resolution in fabricd

`fabricd` SHALL resolve a logical resource name only within the binding set authorized for the
session's tenant, and SHALL refuse to resolve a name outside that tenant's bindings, so
credentials and resources never cross workspace boundaries.

#### Scenario: Name within the tenant's bindings resolves

- **WHEN** a session for tenant A requests a logical name bound for tenant A
- **THEN** the name resolves to tenant A's configured resource

#### Scenario: Name outside the tenant's bindings is refused

- **WHEN** a session for tenant A requests a logical name that is bound only for tenant B
- **THEN** resolution fails and no connection to tenant B's resource is attempted

#### Scenario: Credentials never reach the box

- **WHEN** any resource is resolved for a session
- **THEN** the resolved credentials remain in `fabricd` and only the logical result crosses the wire

### Requirement: Box-direct local egress binding

The box SHALL support resolving a logical resource name **box-direct** (without a broker) to a
co-located service endpoint, when — and only when — the operator declares that binding in the box's
**global configuration**. The binding SHALL NOT be settable per-request and SHALL NOT be influenced by
the executing script. A box-direct target SHALL be restricted to a loopback/private (co-located)
address; a remote target SHALL require a broker. A box-direct call SHALL pass through the same mux
invariants (request allowlist, `meta.io.<name>` metering, deadline, fail-closed) as a broker call.
Resolution order: the name must be in the request `config.io` allowlist; if it is present in the
global local map it resolves box-direct, otherwise it forwards to the broker.

The global local map MAY hold **many** named bindings (e.g. `api1 → http://localhost:8080`,
`api2 → http://localhost:9000`), each addressed by its own logical name. A box-direct call SHALL carry
the **identical** `{action, payload}` envelope a broker receives, so a name is a stable indirection: a
service can be moved between box-direct and broker resolution with no change to the calling script.

#### Scenario: Operator-declared local name resolves box-direct

- **WHEN** the global config binds `"pricing"` to a loopback endpoint and a request lists `"pricing"`
  in `config.io` and calls `$std.io.call("pricing", action, payload)`
- **THEN** the box POSTs `{action, payload}` to the configured local endpoint directly, opening no
  broker session, and the script never sees the endpoint

#### Scenario: Undeclared name falls through to the broker

- **WHEN** a request calls `$std.io.call("orders", …)` and `"orders"` is not in the global local map
- **THEN** the box forwards the name to the broker over uds/quic

#### Scenario: Remote target is not allowed box-direct

- **WHEN** an operator attempts to bind a box-direct name to a non-loopback/remote address
- **THEN** the binding is rejected (a remote logical target must go through a broker)

#### Scenario: Multiple local services under distinct names

- **WHEN** the global config binds `"api1" → http://localhost:8080` and `"api2" → http://localhost:9000`
- **AND** a request lists both in `config.io` and calls `$std.io.call("api1", …)` then `$std.io.call("api2", …)`
- **THEN** each resolves box-direct to its own endpoint, both carrying the same `{action, payload}`
  envelope a broker call would use

#### Scenario: Promote a local service to a broker without touching scripts

- **WHEN** `"api1"` is moved from a box-direct global binding to a broker-resolved name
- **THEN** a script calling `$std.io.call("api1", action, payload)` is unchanged and continues to work

### Requirement: Fail-closed when no egress backend can serve a logical name

The box SHALL refuse with a retryable `503 EGRESS_UNAVAILABLE`, before any egress executes, when a
request addresses an allowlisted `io` logical name but no egress backend can serve it — neither an
operator-declared box-direct `local_resources` binding nor a configured, reachable egress broker
(over UDS or QUIC). The box MUST NOT fall back to any ambient network path. This decision is made at
session-open (before the blocking execution is admitted), so a hung or absent broker is bounded
rather than silently degraded.

This requirement promotes an invariant previously stated only in prose ("fail-closed") into a
testable behavioral contract; it does not change existing behavior.

#### Scenario: Allowlisted name, no broker and no box-direct binding

- **WHEN** a request lists a logical name in `config.io` that resolves neither to a box-direct
  `local_resources` binding nor to any configured egress broker
- **THEN** the box returns `503` with error code `EGRESS_UNAVAILABLE` and executes no egress call

#### Scenario: The refusal is retryable

- **WHEN** the box refuses an `io` request with `EGRESS_UNAVAILABLE`
- **THEN** the response is classified as retryable (`503`, never a client `4xx` and never `429`),
  signalling the caller may retry once a backend becomes reachable

#### Scenario: Refusal precedes execution admission

- **WHEN** an `io`-addressing request has no reachable egress backend
- **THEN** the box decides `EGRESS_UNAVAILABLE` at session-open, before admitting the blocking
  execution, so no runtime is consumed for a request that cannot perform its egress

#### Scenario: Non-egress requests are unaffected

- **WHEN** a request performs no `io` egress (deterministic work, or only `http`/`s3` built-ins)
- **THEN** the absence of a configured egress broker does not cause an `EGRESS_UNAVAILABLE`
  refusal and the request executes normally

