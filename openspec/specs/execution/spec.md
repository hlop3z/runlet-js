# execution Specification

## Purpose

The `/execute` endpoint runs a client-supplied JavaScript `handler(ctx)` inside an isolated
QuickJS context and returns a `{data, error, meta}` envelope. This capability defines the
request/response contract, the handler shape (classic script or ES module), the sandbox
guarantees, and the system-error taxonomy. Rationale: `docs/design/` and `CLAUDE.md`.

## Requirements

### Requirement: Single execution endpoint

The system SHALL expose `POST /execute` as the primary execution endpoint, accepting a JSON
body and returning a JSON `{data, error, meta}` envelope. The system additionally exposes
`POST /batch` (see the `batch-execution` capability), whose per-item results carry
the same `{data, error, meta}` envelope; no other execution endpoint exists.

#### Scenario: Successful execution

- **WHEN** a request supplies a handler that returns `json(value, null)`
- **THEN** the response is HTTP 200 with `data` set to `value`, `error` null, and a `meta` object

#### Scenario: Response always carries the envelope shape

- **WHEN** any request to `/execute` completes (success or failure)
- **THEN** the response body has exactly the keys `data`, `error`, and `meta`

#### Scenario: Batch endpoint reuses the envelope per item

- **WHEN** a request to `/batch` completes
- **THEN** every `results[i]` entry carries the same `{data, error, meta}` envelope defined here

### Requirement: Source resolution (script XOR key)

A request SHALL provide exactly one of an inline `script` or a registered `key`; the engine
path is identical for both.

#### Scenario: Inline script

- **WHEN** the body contains `script` and no `key`
- **THEN** the inline source is executed

#### Scenario: Neither or both provided

- **WHEN** the body contains both `script` and `key`, or neither
- **THEN** the response is HTTP 400 with error code `SCRIPT_XOR_KEY`

#### Scenario: Unknown key

- **WHEN** the body contains a `key` that is not in the script registry
- **THEN** the response is HTTP 404 with error code `SCRIPT_NOT_FOUND`

### Requirement: Handler contract

The source SHALL define a `handler(ctx)` function (or export one); its return value is produced
via the `json(data, error)` bridge and becomes the response `data`/`error`.

#### Scenario: Missing handler

- **WHEN** the source defines no `handler`
- **THEN** the response error code is `HANDLER_NOT_DEFINED`

#### Scenario: Context passed as ctx

- **WHEN** the request supplies a `context` object
- **THEN** the handler receives it as its `ctx` argument; an omitted context defaults to `{}`

### Requirement: Classic-script and ES-module handlers

The system SHALL accept a handler authored as a classic script (`function handler(ctx)`) or as
a native ES module (`export default function handler` or `export function handler`), detecting
module mode by the presence of a top-level `export`.

#### Scenario: Classic script

- **WHEN** the source has no top-level `export` and defines `function handler(ctx)`
- **THEN** it runs in script mode and the handler is invoked

#### Scenario: ES-module handler

- **WHEN** the source has a top-level `export` and exports a handler (default or named)
- **THEN** it runs in module mode, the exported handler is read from the module namespace and invoked

### Requirement: Per-request isolation and sandbox limits

Each execution SHALL run in a fresh QuickJS context with no global scope leaking between
requests, under operator-configured memory, stack, wall-clock timeout, and operation-count
limits, with `eval` and `Proxy` removed before the handler runs. Any cross-request compilation
or bytecode cache SHALL be namespaced by the trusted tenant identity, so identical source from
different tenants never shares a cache entry (no cross-tenant dedup or compile-timing leak).

A wall-clock `TIMEOUT` SHALL take its `retryable` value from the operator-configured
`timeout_retryable` flag (default `true`), because the engine cannot distinguish a transient
slow-dependency stall (retrying helps) from a slow algorithm (retrying wastes budget); it
projects `false ⇒ 422` (park), `true ⇒ 503` (retry). `MEMORY_LIMIT` and `max_ops` are
deterministic for a given `(script, input)` — the same request hits the same limit every time —
so they SHALL be non-retryable (`422`) regardless of the flag.

#### Scenario: No cross-request global leakage

- **WHEN** one request mutates global scope
- **THEN** a subsequent request observes a clean global scope

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

### Requirement: Input validation before execution

The system SHALL reject malformed or oversized input before taking an execution permit. A
malformed body SHALL return `400 MALFORMED_REQUEST`; an oversized script or context SHALL
return `413` (Content Too Large) with its request-category error code. Both statuses fall in
the `4xx` (park) class per the projection requirement.

#### Scenario: Malformed request body

- **WHEN** the body is not valid JSON for `/execute` (bad JSON, wrong field types)
- **THEN** the response is HTTP 400 with error code `MALFORMED_REQUEST` and the same `{data, error, meta}` envelope

#### Scenario: Oversized script or context

- **WHEN** the script or context exceeds its configured size limit
- **THEN** the request is rejected before execution with HTTP `413` and a request-category error code (`SCRIPT_TOO_LARGE` / `CONTEXT_TOO_LARGE`)

### Requirement: System-error taxonomy

On a system-generated failure the `error` SHALL be a structured envelope
(`{type, source, code, message, retryable, owner, details?, debug?}`) the client can branch on
without parsing strings. The HTTP status carrying that envelope SHALL be the projection of its
`(success, retryable)` (see the status-projection requirement), so the status line and the
envelope never contradict each other. Operator-owned non-retryable misconfiguration
(`AUTH_REQUEST`, `S3_FORBIDDEN`, `RESOURCE_KIND_MISMATCH`) SHALL return `409` — a `4xx` (park),
because retrying an unchanged misconfiguration cannot succeed even though the fix is the
operator's.

#### Scenario: Classified engine errors

- **WHEN** execution fails for a known reason (syntax, missing handler, unresolved import, timeout, memory, malformed response, internal)
- **THEN** the error carries a stable `code` (e.g. `SYNTAX_ERROR`, `HANDLER_NOT_DEFINED`, `MODULE_NOT_FOUND`, `TIMEOUT`, `MEMORY_LIMIT`, `MALFORMED_RESPONSE`, `INTERNAL`) with an `owner` and `retryable` hint

#### Scenario: Uncaught handler throw

- **WHEN** the handler throws an error that is not a tagged capability error
- **THEN** the error is classified as a script error owned by the developer

#### Scenario: Status agrees with the envelope

- **WHEN** any system-generated error is returned
- **THEN** the HTTP status class matches the envelope's `retryable` (`5xx` when `true`, `4xx` when `false`)

#### Scenario: Operator misconfiguration parks at 409

- **WHEN** a request fails with an operator-owned non-retryable code (`AUTH_REQUEST`, `S3_FORBIDDEN`, `RESOURCE_KIND_MISMATCH`)
- **THEN** the response status is `409`

### Requirement: Response metadata

Every response SHALL include a `meta` object with a correlation `trace_id`, input sizes,
execution time, and per-capability operation metrics. Per-capability metrics SHALL be
reported under a single dynamic `meta.io` map keyed by capability name — one entry per
capability the request actually used, with the same per-op metric shape for standard and
dev-registered capabilities alike. The former fixed per-capability fields
(`meta.db_requests`, `meta.mail_requests`, …) are removed (**BREAKING**: pre-publish,
single known consumer; tests and typings updated in the same change).

#### Scenario: Trace id present on every response

- **WHEN** any request completes
- **THEN** `meta.trace_id` is a unique id, also logged server-side with the raw cause

#### Scenario: Key echoed in key mode

- **WHEN** a request executes by `key`
- **THEN** `meta.key` echoes the resolved key

#### Scenario: Capability metrics keyed by name

- **WHEN** a request performs operations through capabilities `db` and `custom-nats`
- **THEN** `meta.io.db` and `meta.io["custom-nats"]` each carry that capability's per-op metrics, and no `meta.io` entry exists for unused capabilities

#### Scenario: Legacy fixed metric fields absent

- **WHEN** any request completes after this change
- **THEN** `meta` carries no `<capability>_requests` fields; per-capability metrics appear only under `meta.io`

### Requirement: HTTP capability global name

The in-engine, SSRF-guarded HTTP capability SHALL be exposed to scripts as the global `http`
(previously `api`). The rename aligns the script-facing name with the already-`http` internals
(module, native hook, cargo feature) and with the resource-named convention of every other
capability global. This is **BREAKING** (pre-publish, single known consumer, no alias). Its
operator-supplied gating config key (`allowed_hosts`) is unchanged.

#### Scenario: HTTP calls go through the `http` global

- **WHEN** a script performs an HTTP request in a request whose config permits it
- **THEN** it calls `http.get`/`http.post`/`http.put`/`http.patch`/`http.delete` (method names unchanged), and the global `api` does not exist (`typeof api === "undefined"`)

### Requirement: HTTP status projects the retry action

The HTTP status **class** of an `/execute` response SHALL be a pure function of
`(success, retryable)`, so a consumer routing on the status line alone reaches the same
decision the envelope's `retryable` field implies:

- success (no system error) ⇒ `2xx` (ack)
- `retryable = true` ⇒ `5xx` (retry)
- `retryable = false` ⇒ `4xx` (park / dead-letter)

`retryable` is meaningful only when `success = false`; a successful outcome is always `2xx`
and its `retryable` is undefined. The engine SHALL compute the status class — a script MUST NOT
be able to emit a `2xx` response carrying a system error, i.e. **`2xx` if and only if `error` is
null**. `owner` SHALL NOT change the status class; it only selects which code within the class
(observability and team routing) and continues to ride the response body. All status decisions
SHALL derive from a single projection over the existing `(retryable, owner)` classification, not
from per-call-site status literals.

Within `retryable = true`, the code SHALL be `500` for box-internal (`INTERNAL`) and `503` for
every other retryable failure, **including capacity/quota** (`OVERLOADED`, `PARTITION_OVERLOADED`,
`QUOTA_EXCEEDED`). The status `429` SHALL NOT be used: its `4xx` digit would make a status-line
worker park a retryable response. Every `503` (and `500`) response SHALL carry a `Retry-After`
header, seeded from the relevant circuit-breaker cool-down where one exists and otherwise a
configured default.

#### Scenario: Retryable failure routes to 5xx

- **WHEN** a system-generated error has `retryable = true` (e.g. a dependency outage, a bulkhead rejection, or an exceeded quota)
- **THEN** the response status is `5xx` (`500` for `INTERNAL`, `503` otherwise — never `429`) and carries a `Retry-After` header

#### Scenario: A script cannot emit 2xx with an error

- **WHEN** any response carries a non-null `error` (system-generated or handler-returned)
- **THEN** the status is never `2xx`; `2xx` occurs only when `error` is null

#### Scenario: Non-retryable failure routes to 4xx

- **WHEN** a system-generated error has `retryable = false`
- **THEN** the response status is `4xx` and no automatic retry is signalled

#### Scenario: Owner does not change the status class

- **WHEN** two non-retryable errors differ only in `owner` (e.g. `caller` vs `developer`)
- **THEN** both responses are `4xx` (the specific code may differ) and the `owner` field is carried in the body unchanged

### Requirement: Capability failures reflect their retry classification in the status line

A system-generated `capability` error that reaches the top of a request SHALL project its
`retryable` onto the HTTP status per the projection requirement, rather than always returning
`200`. This covers a driver-backed capability that threw and was not caught by the handler, or
an in-band `http`/`auth` failure surfaced as the request outcome.
This is **BREAKING** relative to the prior contract (capability errors were HTTP 200);
it ships as a clean break (pre-publish, no external consumers, no opt-in or alias). A
capability error that the handler catches and converts into its own returned `error` is a
handler-owned error and follows the handler-declared rule below, not this one.

#### Scenario: Retryable capability outage is not acked

- **WHEN** a handler's uncaught `db` call fails with a retryable code (e.g. `DB_DEADLOCK`, `DB_CONNECTION`, `DB_TIMEOUT`, `DB_CIRCUIT_OPEN`)
- **THEN** the response status is `503` with a `Retry-After` header, and the `{type: "capability", retryable: true, ...}` envelope is unchanged in the body

#### Scenario: Permanent capability failure parks

- **WHEN** a handler's uncaught `db` call fails with a non-retryable code (e.g. `DB_CONSTRAINT`, `DB_QUERY`)
- **THEN** the response status is `4xx` (not `200`) and the envelope reports `retryable: false`

### Requirement: Handler-declared retryability (opt-in)

The system SHALL read a top-level boolean `retryable` key on the handler-returned `error`
object when present and project it onto the HTTP status line (`true ⇒ 503`, `false ⇒ 422`)
without modifying the body, which is otherwise passed through **verbatim** (invariant D1). An
`error` object with **no** `retryable` key SHALL default to `422` (park) — a non-null handler
error is never `2xx`. The body is passed through unchanged in every case.

#### Scenario: Handler opts into retry

- **WHEN** a handler returns `json(null, { message: "...", retryable: true })`
- **THEN** the response status is `503` (with `Retry-After`) and the `error` body is exactly what the handler returned

#### Scenario: Handler opts into park

- **WHEN** a handler returns `json(null, { message: "...", retryable: false })`
- **THEN** the response status is `422` and the `error` body is exactly what the handler returned

#### Scenario: Un-annotated handler error defaults to park

- **WHEN** a handler returns `json(null, { message: "name required" })` with no `retryable` key
- **THEN** the response status is `422` and the `error` body is passed through unchanged
