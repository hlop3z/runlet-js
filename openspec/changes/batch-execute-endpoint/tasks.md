# Tasks: batch-execute-endpoint

## 1. Endpoint + fan-out

- [x] 1.1 Add `max_batch_items` + combined-input-bytes cap + total-response-bytes cap to server config (defaults per design D3/D6)
- [x] 1.2 Add `POST /batch` route (top-level, not nested under `/execute`) + batch request/response types (order-preserving `results`, optional per-item client `id` echoed on each result per D7, batch `meta` with `items/ok/failed/duration_ms/trace_id`)
- [x] 1.3 Batch-level validation before any admission: malformed body, empty batch, item count, combined input bytes → HTTP 400 request-category error
- [x] 1.4 Per-item fan-out over the existing path (validate → bulkhead admit → `spawn_blocking` → `LogicHost::run`); per-item errors land in `results[i].error`, batch returns HTTP 200 (design D4)
- [x] 1.5 D6 response-size guard: enforce the total-response-bytes cap while assembling `results`; on exceed, truncate the offending item to a classified size-limit error envelope (default) rather than buffering unbounded output

## 2. Accounting + trusted mode

- [x] 2.1 Per-item quota debit through the existing atomic quota path; boundary behavior (N remaining, N+1 items) matches the spec scenario
- [x] 2.2 Per-item usage + audit events; batch adds a parent span + batch `trace_id` only
- [x] 2.3 Trusted-mode: batch shares the request's tenant identity across items; fairness keys per item off that tenant; capability/entitlement authz (`authz.rs`) is evaluated per item, not once for the batch (D5 — GraphQL-batch-attack guard)

## 3. Verification + docs

- [x] 3.1 Integration suite batch section (`tests/test_simple.py` `test_batch` + `_post_batch`): order preservation, per-item `id` echo, partial failure + isolation, single-execute envelope shape, caps (empty / over-max / malformed-item), response-size cap (item truncated to `BATCH_RESPONSE_TRUNCATED`). Per-item quota/authz count==N and events-count are covered by the deterministic Rust unit tests (`handler.rs` `batch_tests`, incl. the D5 per-item authz test) since they need the trusted-mode box; the Python section runs against the default box.
- [x] 3.2 Fairness check: a large slow batch on one partition does not starve another partition's single request (added to the `/batch` section, mirroring `test_partition_fairness`)
- [x] 3.3 Saturation bench (`crates/runlet-bench/benches/batch.rs`): per-item fan-out cost across batch sizes {1,10,25,100} — characterizes the linear per-item cost that bounds `max_items × per-item-time`. Default kept at D3's `25`; run `cargo bench -p runlet-bench --bench batch` to revisit empirically.
- [x] 3.4 Docs: README `### Batch execution` section (independence/no-atomicity/per-item/bounds called out) + operational-endpoints mention, `container/types.d.ts` batch envelope (via `base.d.ts`, D11 golden test green), `docs/README.md` beginner mention, CLAUDE.md request-lifecycle note
