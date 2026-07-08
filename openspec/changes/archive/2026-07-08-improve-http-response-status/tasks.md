## 1. Config knob

- [x] 1.1 Add `timeout_retryable: bool` (default `true`) to the server `Config`/`EngineConfig` in `crates/runlet/src/config.rs`, with parse + default + doc comment
- [x] 1.2 Thread `timeout_retryable` down to where the `TIMEOUT` fault is constructed (`crates/runlet-core/src/engine.rs`) so its `retryable` reflects the flag; keep `MEMORY_LIMIT` and `max_ops` non-retryable (deterministic) regardless of the flag
- [x] 1.3 Add/adjust the `Retry-After` default seconds config (used when no breaker cool-down applies)

## 2. Single projection function

- [x] 2.1 Introduce one `http_status(fault) -> (StatusCode, Option<RetryAfter>)` (name TBD) as the single source of truth, driven off `(retryable, owner, code-class)` per design D1/D2/D6
- [x] 2.2 Implement the class rule: `2xx` iff `error` is null; `retryable=true ⇒ 5xx`, `retryable=false ⇒ 4xx`; `owner` selects only the code within the class (never the class)
- [x] 2.3 Implement the `5xx` split: `500` for `INTERNAL`, `503` for every other retryable (incl. capacity/quota `OVERLOADED`/`PARTITION_OVERLOADED`/`QUOTA_EXCEEDED`); **never emit `429`**; return `Retry-After` on `503`/`500`
- [x] 2.4 Implement the `4xx` codes: oversize → `413`, operator-non-retryable misconfig (`AUTH_REQUEST`, `S3_FORBIDDEN`, `RESOURCE_KIND_MISMATCH`) → `409`, caller → `400`/`404`, developer/runtime → `422`
- [x] 2.5 Enforce non-contradiction structurally (D7): `Fault` sole constructor, `code`→`(retryable,owner)` from the catalog, no ad-hoc `Fault` literals or hand-rolled statuses
- [x] 2.6 Unit-test the projection function as a pure table (every code-class → expected status + Retry-After presence); assert `429` is never produced

## 3. Wire the projection into the request paths

- [x] 3.1 Replace `EngineError::http_status()` in `crates/runlet-core/src/engine.rs` with a call into the projection function
- [x] 3.2 Collapse the per-condition status builders in `crates/runlet/src/handler.rs` to use the projection function
- [x] 3.3 Remove the `EngineError::Capability ⇒ 200` special case so uncaught capability failures project (D3, BREAKING)
- [x] 3.4 Emit the `Retry-After` response header on `503`/`500`, seeded from the per-target circuit-breaker cool-down when open, else the configured default
- [x] 3.5 Confirm `/batch` is untouched — an admitted batch stays `200`-with-envelope; only per-item envelopes carry classification

## 4. Handler-declared retryability (opt-in)

- [x] 4.1 In the handler-envelope parse path (`struct Envelope` in `handler.rs`), detect a top-level boolean `retryable` key on the returned `error` object without altering the body; enforce `2xx` iff `error` is null
- [x] 4.2 Project it to the status line only: `true ⇒ 503` (+ `Retry-After`), `false ⇒ 422`; **absent ⇒ `422` (park)**, not `200`
- [x] 4.3 Verify the `error` body is passed through verbatim in all three cases (D1/D5)

## 5. Docs

- [x] 5.1 Update the traffic-light table in `docs/99-errors.md` to the new projection (capability errors off 200; 413/409; all retryable → 503/500, no 429; `Retry-After`)
- [x] 5.2 Update the per-code `retry`/`owner` tables; note `timeout_retryable` governs only `TIMEOUT` (default `true`), and `MEMORY_LIMIT`/`max_ops` stay non-retryable
- [x] 5.3 Document the handler opt-in `retryable` key and that `/batch` consumers are envelope-readers
- [x] 5.4 Keep `README.md` reference copy in sync

## 6. Tests

- [x] 6.1 Update `tests/test_simple.py` assertions: capability failures now assert their projected `4xx`/`5xx` (not 200)
- [x] 6.2 Add assertions for `Retry-After` on `503`/`500`; assert quota/overload return `503` (not `429`)
- [x] 6.3 Add assertions for oversize → `413` and operator-misconfig → `409`
- [x] 6.4 Add a `timeout_retryable` matrix: `true ⇒ 503`, `false ⇒ 422` for `TIMEOUT`; assert `MEMORY_LIMIT`/`max_ops` stay `422` under both
- [x] 6.5 Add handler opt-in cases: `retryable:true ⇒ 503`, `retryable:false ⇒ 422`, absent ⇒ `422` (park), body verbatim in all; assert no `2xx` with a non-null `error`
- [x] 6.6 Assert status/envelope agreement (`5xx ⇔ retryable:true`, `4xx ⇔ retryable:false`) across a representative sample

## 7. Gate

- [x] 7.1 `task fmt-check` clean
- [x] 7.2 `task clippy` clean (re-run until no errors surface)
- [x] 7.3 `cargo test` + the updated `tests/test_simple.py` pass (Docker build path)
