## ADDED Requirements

### Requirement: Targeted local-service allowlist

The `http` capability SHALL let an operator allowlist specific local host:port targets (e.g.
`localhost:8000`) such that an **explicitly named** target bypasses the private/internal-IP block,
without relaxing the block for any other target. This SHALL be distinct from the `debug` switch, which
relaxes the private-IP block globally and remains a development-only knob. An allowlisted local target
SHALL still be subject to every other `http` guard (host allowlist match, redirect re-validation).

#### Scenario: Named local target is reachable in production

- **WHEN** `http.allowed_hosts` includes `localhost:8000` and `debug` is off
- **THEN** an `http` request to `http://localhost:8000/...` is permitted

#### Scenario: Un-named local target is still blocked

- **WHEN** `http.allowed_hosts` includes `localhost:8000` and `debug` is off
- **AND** a script requests `http://localhost:9999/...`
- **THEN** the request is blocked by the private-IP guard

#### Scenario: `debug` remains the blanket relax

- **WHEN** `debug` is on
- **THEN** the private-IP block is relaxed globally (development only), independent of the allowlist
