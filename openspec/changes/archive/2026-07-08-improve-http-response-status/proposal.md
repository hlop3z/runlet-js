## Why

The `/execute` response envelope already classifies every system error with `retryable` and
`owner`, but the HTTP **status line** does not faithfully project that classification: the
entire `capability` bucket (db/redis/amq/mail/s3/auth failures) returns **HTTP 200** — including
the retryable ones. A queue worker or generic retry middleware that routes on the status line
alone therefore treats a transient Postgres deadlock as a successful ack and **drops a message
that would have succeeded on retry**. We want the status line to be a truthful projection of the
envelope so dumb infrastructure can route correctly (`4xx` park, `5xx` retry, `2xx` ack), while
envelope-reading clients gain a status that never contradicts the body.

## What Changes

- Establish one invariant: **the HTTP status *class* is a pure function of `(success, retryable)`** —
  `success ⇒ 2xx`, `retryable=true ⇒ 5xx`, `retryable=false ⇒ 4xx`, with **`2xx` if and only if
  `error` is null** (the engine owns the class; a script cannot emit a `2xx` carrying an error).
  `owner` only selects *which* code within the class (observability + team routing) and never
  changes the class.
- **BREAKING**: system-generated `capability` errors (db/redis/amq/mail/s3/auth and in-band
  `http`/`auth` failures that reach the top of a request) now project onto the status line:
  retryable → `503` / `500` (box-internal); non-retryable → `4xx`. Previously all were HTTP 200.
  Pre-publish, no external consumers, so shipped as a clean break (no opt-in, no alias).
- Route `retryable=true` responses to `503` for everything transient (dependency outages,
  capacity/quota — `OVERLOADED`, `PARTITION_OVERLOADED`, `QUOTA_EXCEEDED`) and `500` for
  `INTERNAL`, always with a `Retry-After` header (seeded from the circuit-breaker cool-down where
  one exists). **`429` is not used** — its `4xx` digit would make a status-line worker park a
  retryable response.
- Reclassify a few existing codes to obey the invariant: oversize input → **413** (was 400);
  operator-non-retryable misconfig (`AUTH_REQUEST`, `S3_FORBIDDEN`, `RESOURCE_KIND_MISMATCH`) → **409**.
- **`TIMEOUT` becomes config-driven**: a new `config.timeout_retryable: bool` (default `true`) sets
  its `retryable`, which crosses the status boundary (`true ⇒ 503 retry`, `false ⇒ 422 park`). The
  box cannot distinguish a slow algorithm from a slow dependency, so this is an operator knob;
  default retry because the ladder bounds a runaway while park-by-default risks dropping recoverable
  messages. `MEMORY_LIMIT` and `max_ops` are deterministic and stay non-retryable (`422`).
- **Handler-declared retry (opt-in)**: the box reads a well-known boolean `retryable` key on the
  handler's returned `error` and projects it (`true ⇒ 503`, `false ⇒ 422`); an un-annotated handler
  error defaults to **`422` (park)**, not `200`. The `error` body is still passed through
  **verbatim** — D1 (opaque passthrough) bends to *read* one key, never to rewrite the body.
- **Non-contradiction is structural**: `Fault` is the sole constructor and `code` comes from a
  catalog carrying its `(retryable, owner)`, so a status that contradicts the class is
  unrepresentable — no ad-hoc `Fault` literals or hand-rolled statuses.
- **`/batch` is explicitly out of scope for status projection**: an admitted batch has per-item
  outcomes that cannot share one status line, so it stays `200`-with-envelope. Batch consumers
  are envelope-readers by construction; this is documented, not changed.
- All status decisions flow through a single projection function (one source of truth), driven
  off the existing `(retryable, owner)` — no per-call-site status literals.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `execution`: adds an **HTTP status = projection of `(success, retryable)`** requirement; changes
  the `TIMEOUT`/`MEMORY_LIMIT` scenarios to be `timeout_retryable`-driven; changes capability-error
  responses from HTTP 200 to their projected `4xx`/`5xx`; reclassifies oversize → 413 and
  operator-misconfig → 409; adds the `Retry-After` header and the handler-declared `retryable`
  opt-in.

## Impact

- **Code**: `crates/runlet/src/handler.rs` (status builders → single projection fn), the
  `EngineError::http_status()` mapping in `crates/runlet-core/src/engine.rs`, the top-of-request
  handling of `EngineError::Capability` (currently forced to 200), `Retry-After` header emission,
  handler-envelope parsing (`struct Envelope`) to read the opt-in `retryable` key, and
  `crates/runlet/src/config.rs` for `timeout_retryable`.
- **Contract**: `crates/runlet-wire` types (`Fault`/`retryable`/`owner`) are unchanged — this is a
  *projection* of existing fields, not a wire change.
- **Docs**: the traffic-light table and per-code tables in `docs/99-errors.md` need updating; the
  `execution` spec delta captures the normative behavior.
- **Tests**: `tests/test_simple.py` status-code assertions for capability failures move off 200;
  new assertions for `Retry-After`, 413/409, the `timeout_retryable` knob, and the handler opt-in.
- **BREAKING** but pre-publish with no external consumers; `/batch` behavior is unchanged.
