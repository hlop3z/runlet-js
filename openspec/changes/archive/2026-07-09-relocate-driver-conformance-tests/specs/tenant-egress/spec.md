## ADDED Requirements

### Requirement: Fail-closed when no egress backend can serve a logical name

The box SHALL refuse with a retryable `503 EGRESS_UNAVAILABLE`, before any egress executes, when a
request addresses an allowlisted `io` logical name but no egress backend can serve it — neither an
operator-declared box-direct `local_resources` binding nor a configured, reachable `fabricd` sidecar
(over UDS or QUIC). The box MUST NOT fall back to any ambient network path. This decision is made at
session-open (before the blocking execution is admitted), so a hung or absent broker is bounded
rather than silently degraded.

This requirement promotes an invariant previously stated only in prose ("fail-closed") into a
testable behavioral contract; it does not change existing behavior.

#### Scenario: Allowlisted name, no sidecar and no box-direct binding

- **WHEN** a request lists a logical name in `config.io` that resolves neither to a box-direct
  `local_resources` binding nor to any configured `fabricd` sidecar
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
- **THEN** the absence of a configured `fabricd` sidecar does not cause an `EGRESS_UNAVAILABLE`
  refusal and the request executes normally
