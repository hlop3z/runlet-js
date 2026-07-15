## ADDED Requirements

### Requirement: Contract verification is opt-in and layered

The system SHALL treat signed-contract verification as an **opt-in sub-mode** of trusted-header
mode, activated only when the operator enables it. When the sub-mode is disabled, request handling
SHALL be unchanged and no JWKS acquisition, signature verification, or contract-claim reading SHALL
occur — the generic trusted-header path continues to serve a non-nexus edge that injects plain
headers.

#### Scenario: Sub-mode disabled leaves handling unchanged

- **WHEN** trusted-header mode is enabled but the contract sub-mode is disabled
- **THEN** no `x-identity-contract` verification occurs and identity is derived exactly as in the
  plain trusted-header path

#### Scenario: Sub-mode enabled activates verification

- **WHEN** the contract sub-mode is enabled and an identity-enriched request arrives
- **THEN** the system verifies the request's `x-identity-contract` before any egress session or
  execution begins

### Requirement: Enabling the sub-mode requires complete configuration

When the contract sub-mode is enabled, the system SHALL require a JWKS source URL, an expected
issuer, and an expected audience (this box's pool name) to be configured, and SHALL refuse to start
with a configuration error if any is missing.

#### Scenario: Missing JWKS source refuses to start

- **WHEN** the contract sub-mode is enabled but no JWKS source URL is configured
- **THEN** the process refuses to start with a configuration error

#### Scenario: Missing issuer or audience refuses to start

- **WHEN** the contract sub-mode is enabled but the expected issuer or audience is not configured
- **THEN** the process refuses to start with a configuration error

### Requirement: Signature verification against rotating JWKS

When the sub-mode is enabled, the system SHALL verify the `x-identity-contract` as a compact ES256
JWS: it SHALL select the verifying key by the token header's `kid` from a JWKS fetched from the
configured source, SHALL cache the JWKS, and SHALL refresh it on an unknown `kid` (keys rotate with
overlap). A token whose signature does not verify against the selected key SHALL be rejected. JWKS
acquisition SHALL NOT introduce a second cryptographic stack — verification reuses the box's
existing crypto provider (no `ring`, no OpenSSL/`aws-lc-sys`).

#### Scenario: Valid signature verifies

- **WHEN** a request carries a contract signed by a key present in the current JWKS and selected by `kid`
- **THEN** the signature verifies and claim checks proceed

#### Scenario: Unknown kid triggers a refresh then verifies

- **WHEN** a contract's `kid` is not in the cached JWKS
- **THEN** the system refreshes the JWKS and, if the key is then present, verifies against it

#### Scenario: Bad signature is rejected

- **WHEN** a contract's signature does not verify against the key selected by its `kid`
- **THEN** the request is rejected with an authorization failure and no handler runs

### Requirement: Registered-claim validation

When the sub-mode is enabled, the system SHALL reject a verified-signature contract unless all of the
following hold: `iss` equals the configured expected issuer; `aud` equals the configured expected
audience (this box's pool); and `exp` is in the future within a small configured clock-skew leeway.
A repeated `jti` across requests SHALL NOT be treated as a replay or error (the edge may reuse one
contract within a short window).

#### Scenario: Issuer mismatch is rejected

- **WHEN** a contract's `iss` does not equal the configured issuer
- **THEN** the request is rejected with an authorization failure

#### Scenario: Audience-for-another-box is rejected

- **WHEN** a contract's `aud` names a different box than this box's configured audience
- **THEN** the request is rejected with an authorization failure

#### Scenario: Expired contract is rejected

- **WHEN** a contract's `exp` is in the past beyond the configured leeway
- **THEN** the request is rejected with an authorization failure

#### Scenario: Repeated jti is not a replay

- **WHEN** two requests carry contracts with the same `jti` and unexpired `exp`
- **THEN** both are accepted on the `jti` basis (subject to the other checks)

### Requirement: Contract version drift gate

When the sub-mode is enabled, the system SHALL reject a contract whose `ctr` claim is not in the
configured supported-version set (default containing `v1`), so an incompatible contract-shape change
fails loud rather than being silently mis-read.

#### Scenario: Supported version is accepted

- **WHEN** a contract's `ctr` is in the configured supported set
- **THEN** verification proceeds

#### Scenario: Unknown version is rejected

- **WHEN** a contract's `ctr` is not in the configured supported set
- **THEN** the request is rejected with an authorization failure and the rejection reason identifies
  a contract-version mismatch

### Requirement: Fail closed on an enriched route

When the sub-mode is enabled, the system SHALL reject an identity-enriched request that carries no
`x-identity-contract` or an unverifiable one, before any egress session or execution begins — a
missing or invalid contract is never treated as an anonymous-but-allowed request.

#### Scenario: Missing contract on an enriched route is rejected

- **WHEN** the sub-mode is enabled and an identity-enriched request arrives with no `x-identity-contract`
- **THEN** the request is rejected with an authorization failure and no handler runs

### Requirement: Freshness bound — no caching past expiry

The system SHALL NOT reuse a verified contract's claims for a request beyond that contract's `exp`;
each request's revocation-sensitive signals SHALL be read from a contract valid for that request.

#### Scenario: Claims are not honored past expiry

- **WHEN** a previously verified contract's `exp` has passed
- **THEN** its claims are not reused and the current request's own contract is verified afresh
