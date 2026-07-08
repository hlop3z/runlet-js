## ADDED Requirements

### Requirement: /execute response carries the effects channel

The `/execute` response SHALL surface the ordered `effects` list — each entry a
`{ kind, value }` produced by `emit(kind, value)` — on **both** the success and error paths.
These fields are additive: the existing `data`, `error`, and `meta` fields and the HTTP
status-class projection over `(success, retryable)` are unchanged. A handler that never calls
`emit` SHALL receive a well-formed response with an empty or absent `effects` list, otherwise
identical to the prior `{data, error, meta}` contract.

#### Scenario: A successful run surfaces its effects

- **WHEN** a handler calls `emit("decided", d)` and returns normally
- **THEN** the 2xx response carries `effects` including `{kind:"decided", value:d}` alongside the usual `data`/`error`/`meta`

#### Scenario: A failing run still surfaces effects emitted before the failure

- **WHEN** a handler emits some effects and then produces a system error
- **THEN** the non-2xx response still carries those effects, so a consumer keeps the partial trail

#### Scenario: A run that never emits is unchanged

- **WHEN** an `/execute` request runs a handler that never calls `emit`
- **THEN** the response is well-formed with an empty or absent `effects` list and is otherwise identical to the prior `{data, error, meta}` contract
