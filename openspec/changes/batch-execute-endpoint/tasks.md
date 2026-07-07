# Tasks: batch-execute-endpoint

## 1. Endpoint + fan-out

- [ ] 1.1 Add `max_batch_items` + combined-bytes cap to server config (defaults per design D3)
- [ ] 1.2 Add `POST /execute/batch` route + batch request/response types (order-preserving `results`, batch `meta` with `items/ok/failed/duration_ms/trace_id`)
- [ ] 1.3 Batch-level validation before any admission: malformed body, empty batch, item count, combined bytes → HTTP 400 request-category error
- [ ] 1.4 Per-item fan-out over the existing path (validate → bulkhead admit → `spawn_blocking` → `LogicHost::run`); per-item errors land in `results[i].error`, batch returns HTTP 200 (design D4)

## 2. Accounting + trusted mode

- [ ] 2.1 Per-item quota debit through the existing atomic quota path; boundary behavior (N remaining, N+1 items) matches the spec scenario
- [ ] 2.2 Per-item usage + audit events; batch adds a parent span + batch `trace_id` only
- [ ] 2.3 Trusted-mode: batch shares the request's tenant identity across items; fairness keys per item off that tenant

## 3. Verification + docs

- [ ] 3.1 Integration suite batch section: order preservation, partial failure (TIMEOUT item + successes), isolation between items, caps (oversize/empty/malformed-item), quota debit count, events count
- [ ] 3.2 Fairness check: a large batch from tenant A does not raise tenant B's latency (extend the partition-fairness test)
- [ ] 3.3 Bench a saturating batch to pick the default `max_batch_items` (design open question)
- [ ] 3.4 Docs: README endpoint section (independence/no-atomicity called out), `container/types.d.ts` batch envelope, CLAUDE.md request-lifecycle note
