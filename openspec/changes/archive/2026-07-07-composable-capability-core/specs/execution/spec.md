# execution Specification (delta)

## MODIFIED Requirements

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
