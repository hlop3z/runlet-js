## MODIFIED Requirements

### Requirement: Per-request isolation and sandbox limits

Each execution SHALL run in a fresh QuickJS context with no global scope leaking between
requests, under operator-configured memory, stack, wall-clock timeout, and operation-count
limits, with `eval` and `Proxy` removed before the handler runs. Any cross-request compilation
or bytecode cache SHALL be namespaced by the trusted tenant identity, so identical source from
different tenants never shares a cache entry (no cross-tenant dedup or compile-timing leak).

The engine MAY precompile the **injected framework and value-util surface** (the standard
library scaffolding, bridges, capability wrappers, and value-util wrappers it injects into every
context) to bytecode once and load that bytecode into each fresh context. Such precompiled
injected code SHALL be reused as **compiled code only, never as retained state**: every execution
still receives a fresh context whose global scope reflects no prior request. Because the injected
surface is operator-fixed and identical for all tenants, its bytecode is NOT tenant-scoped and
carries no per-tenant or per-request data — only tenant-submitted *handler* source remains subject
to the tenant-namespaced cache rule above.

A wall-clock `TIMEOUT` SHALL take its `retryable` value from the operator-configured
`timeout_retryable` flag (default `true`), because the engine cannot distinguish a transient
slow-dependency stall (retrying helps) from a slow algorithm (retrying wastes budget); it
projects `false ⇒ 422` (park), `true ⇒ 503` (retry). `MEMORY_LIMIT` and `max_ops` are
deterministic for a given `(script, input)` — the same request hits the same limit every time —
so they SHALL be non-retryable (`422`) regardless of the flag.

#### Scenario: No cross-request global leakage

- **WHEN** one request mutates global scope
- **THEN** a subsequent request observes a clean global scope

#### Scenario: Precompiled injected surface does not leak state across requests

- **WHEN** a request mutates a global or a prototype reachable through the injected framework/value-util surface, and a subsequent request runs against a context built from the same precompiled injected bytecode
- **THEN** the subsequent request observes the pristine injected surface, identical to a context parsed fresh from source (bytecode reuse restores compiled code, not the prior request's mutations)

#### Scenario: Wall-clock timeout

- **WHEN** a handler runs past the configured `timeout_ms`
- **THEN** execution is interrupted and the response error code is `TIMEOUT`

#### Scenario: Timeout retryability follows config

- **WHEN** a handler hits `TIMEOUT`
- **THEN** the error's `retryable` equals `config.timeout_retryable` (default `true`), and the status is `503` (with `Retry-After`) when `true` or `422` when `false`

#### Scenario: Memory and op-limit failures always park

- **WHEN** a request hits `MEMORY_LIMIT` or a `max_ops` operation cap
- **THEN** the error is non-retryable and the status is `422`, regardless of `config.timeout_retryable`

#### Scenario: Operation cap

- **WHEN** a handler exceeds `max_ops` external operations
- **THEN** the offending capability call fails with an operation-limit error

#### Scenario: Compilation cache does not cross tenants

- **WHEN** two different tenants submit byte-identical source
- **THEN** each tenant's compilation is cached under its own tenant namespace and neither observes the other's cache entry
