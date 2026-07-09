## MODIFIED Requirements

### Requirement: Logical resource names on the egress session

Egress SHALL address a **flat list of logical resource names**. The request SHALL declare the names
it may touch as a plain allowlist (`config.io: ["orders", "cache"]`); the box SHALL forward the
selected names to the broker in the session handshake as `resources` (a list of names), never as
per-kind fields. The resource *kind* and *transport* SHALL be resolved entirely by the broker
(operator-side); the box SHALL remain kind-blind and hold no backend endpoint or credential (Model 1:
the box's only egress peer is the broker, reached over UDS locally or QUIC remotely). A name absent
from `config.io` SHALL be rejected.

#### Scenario: Script addresses a logical name only

- **WHEN** a script calls `io.call("orders", "query", payload)` and `"orders"` is in `config.io`
- **THEN** the box forwards the name `"orders"` to the broker, which resolves its kind/endpoint/creds

#### Scenario: Unlisted name is rejected

- **WHEN** a script calls `io.call("secret", …)` and `"secret"` is not in `config.io`
- **THEN** the call is rejected (`RESOURCE_NOT_FOUND`) before any egress

#### Scenario: Handshake carries a flat name list

- **WHEN** a request that names egress resources opens a broker session
- **THEN** the handshake carries `resources` as a flat list of logical names (no per-kind slots)

### Requirement: Tenant identity carried on the egress session

The box SHALL include the request's trusted tenant identity in the broker session handshake when
opening an egress session, so the broker can scope resolution to that tenant. The tenant identity
SHALL never be sourced from a value the executing script can influence. A session carrying a trusted
tenant identity is the multitenant context; the box adds no separate privilege signal (the broker
derives least-privilege enforcement from the tenant identity — carried from `resource-privilege-guard`).

#### Scenario: Session opens with the trusted tenant id

- **WHEN** a request that names egress resources opens a broker session
- **THEN** the handshake carries the request's trusted tenant identity alongside the `resources` list

## ADDED Requirements

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
  in `config.io` and calls `io.call("pricing", action, payload)`
- **THEN** the box POSTs `{action, payload}` to the configured local endpoint directly, opening no
  broker session, and the script never sees the endpoint

#### Scenario: Undeclared name falls through to the broker

- **WHEN** a request calls `io.call("orders", …)` and `"orders"` is not in the global local map
- **THEN** the box forwards the name to the broker over uds/quic

#### Scenario: Remote target is not allowed box-direct

- **WHEN** an operator attempts to bind a box-direct name to a non-loopback/remote address
- **THEN** the binding is rejected (a remote logical target must go through a broker)

#### Scenario: Multiple local services under distinct names

- **WHEN** the global config binds `"api1" → http://localhost:8080` and `"api2" → http://localhost:9000`
- **AND** a request lists both in `config.io` and calls `io.call("api1", …)` then `io.call("api2", …)`
- **THEN** each resolves box-direct to its own endpoint, both carrying the same `{action, payload}`
  envelope a broker call would use

#### Scenario: Promote a local service to a broker without touching scripts

- **WHEN** `"api1"` is moved from a box-direct global binding to a broker-resolved name
- **THEN** a script calling `io.call("api1", action, payload)` is unchanged and continues to work
