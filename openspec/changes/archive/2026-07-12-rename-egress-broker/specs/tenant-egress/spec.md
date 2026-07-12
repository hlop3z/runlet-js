## MODIFIED Requirements

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
