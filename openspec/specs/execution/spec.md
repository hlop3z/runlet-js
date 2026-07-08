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

#### Scenario: No cross-request global leakage

- **WHEN** one request mutates global scope
- **THEN** a subsequent request observes a clean global scope

#### Scenario: Wall-clock timeout

- **WHEN** a handler runs past the configured `timeout_ms`
- **THEN** execution is interrupted and the response error code is `TIMEOUT`

#### Scenario: Operation cap

- **WHEN** a handler exceeds `max_ops` external operations
- **THEN** the offending capability call fails with an operation-limit error

#### Scenario: Compilation cache does not cross tenants

- **WHEN** two different tenants submit byte-identical source
- **THEN** each tenant's compilation is cached under its own tenant namespace and neither observes the other's cache entry

### Requirement: Input validation before execution

The system SHALL reject malformed or oversized input before taking an execution permit.

#### Scenario: Malformed request body

- **WHEN** the body is not valid JSON for `/execute` (bad JSON, wrong field types)
- **THEN** the response is HTTP 400 with error code `MALFORMED_REQUEST` and the same `{data, error, meta}` envelope

#### Scenario: Oversized script or context

- **WHEN** the script or context exceeds its configured size limit
- **THEN** the request is rejected with a request-category error before execution

### Requirement: System-error taxonomy

On a system-generated failure the `error` SHALL be a structured envelope
(`{type, source, code, message, retryable, owner, details?, debug?}`) the client can branch on
without parsing strings.

#### Scenario: Classified engine errors

- **WHEN** execution fails for a known reason (syntax, missing handler, unresolved import, timeout, memory, malformed response, internal)
- **THEN** the error carries a stable `code` (e.g. `SYNTAX_ERROR`, `HANDLER_NOT_DEFINED`, `MODULE_NOT_FOUND`, `TIMEOUT`, `MEMORY_LIMIT`, `MALFORMED_RESPONSE`, `INTERNAL`) with an `owner` and `retryable` hint

#### Scenario: Uncaught handler throw

- **WHEN** the handler throws an error that is not a tagged capability error
- **THEN** the error is classified as a script error owned by the developer

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
