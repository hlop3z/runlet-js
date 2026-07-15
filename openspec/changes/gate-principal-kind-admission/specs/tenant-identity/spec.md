## ADDED Requirements

### Requirement: Box-wide principal-kind admission

The system SHALL support restricting a box to a configured set of principal kinds via a box-wide
allowlist (`trusted.allowed_principal_kinds`), so an operator can dedicate an instance to one class of
principal (a user-box, a service-box) that scales and is routed independently. When the allowlist is
non-empty, the system SHALL admit a request only if its verified `principal_kind` is a member of the
allowlist, and SHALL otherwise reject it with `403 PRINCIPAL_KIND_FORBIDDEN`. The decision SHALL be a
pure function of `principal_kind` — `on_behalf_of` SHALL NOT promote an api-key principal to a human for
this gate. An **empty allowlist SHALL admit every kind** (the default, preserving prior behavior). The
check SHALL run at caller admission (alongside the anonymous / suspended / tenant-required checks),
evaluated once per request and once per batch, and SHALL NOT be evaluated on the per-capability path
(the coarse member-capability gate is unaffected).

#### Scenario: Kind in the allowlist is admitted

- **WHEN** `allowed_principal_kinds` is `["service"]` and a request carries a verified contract whose `principal_kind` is `service`
- **THEN** the principal-kind gate admits the request and execution proceeds to the remaining gates

#### Scenario: Kind absent from the allowlist is rejected

- **WHEN** `allowed_principal_kinds` is `["service"]` and a request carries a verified contract whose `principal_kind` is `user`
- **THEN** the system rejects the request with `403 PRINCIPAL_KIND_FORBIDDEN` and records an audit denial

#### Scenario: Absent principal kind fails closed

- **WHEN** `allowed_principal_kinds` is non-empty and a request's `principal_kind` is absent (no verified contract populated it)
- **THEN** the system rejects the request with `403 PRINCIPAL_KIND_FORBIDDEN`, because an absent kind is never a member of a non-empty allowlist

#### Scenario: Empty allowlist admits every kind

- **WHEN** `allowed_principal_kinds` is empty (the default)
- **THEN** the principal-kind gate admits the request regardless of its `principal_kind`, including an absent kind

#### Scenario: on_behalf_of does not promote an api-key across the gate

- **WHEN** `allowed_principal_kinds` is `["user"]` and a request carries a verified contract whose `principal_kind` is `apikey` with an `on_behalf_of` human subject present
- **THEN** the system rejects the request with `403 PRINCIPAL_KIND_FORBIDDEN`, because the gate decides on `principal_kind` alone

#### Scenario: A configured allowlist without the contract sub-mode refuses to start

- **WHEN** `allowed_principal_kinds` is non-empty but `trusted.contract.enabled` is false
- **THEN** the box refuses to start with a boot-guard error, because `principal_kind` is only ever populated by a verified contract and the box would otherwise deny every request
