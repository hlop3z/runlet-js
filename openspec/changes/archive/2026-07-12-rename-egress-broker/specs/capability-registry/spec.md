## MODIFIED Requirements

### Requirement: Per-name egress routing with fallback

The host SHALL route each capability call to the backend registered for that capability's
name. Names without a locally registered backend SHALL route to the configured fallback
egress (e.g. the broker) when one is wired. A driver-backed call whose name has neither a
local backend nor a fallback SHALL fail with `EGRESS_UNAVAILABLE`.

#### Scenario: Local and remote backends coexist

- **WHEN** a host registers an in-process backend for `db` and wires a broker fallback, and one request calls `db` and `amq`
- **THEN** `db` calls are served in-process and `amq` calls are served through the fallback egress

#### Scenario: No backend and no fallback

- **WHEN** a request calls a registered capability whose name has no local backend and no fallback egress is wired
- **THEN** the call fails with error code `EGRESS_UNAVAILABLE`
