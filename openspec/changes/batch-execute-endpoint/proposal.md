# Proposal: batch-execute-endpoint

## Why

High-volume callers (per-row ETL transforms, webhook fan-out, bulk validations) today choose
between N HTTP round-trips or a user-space loop inside one handler — the loop shares one
QuickJS context, one wall-clock budget, and one `meta` blob, so one bad row kills the batch
and per-item metering/billing is lost. A batch endpoint sells exactly what the loop cannot:
per-item isolation, per-item timeouts and error envelopes, per-item billing dims, and
fairness-aware parallelism — with amortized HTTP overhead.

## What Changes

- New `POST /batch` endpoint (top-level sibling of `/execute`, not nested under it):
  `{ items: [ {script|key, context, config, id?}, … ] }` →
  `{ results: [ {data,error,meta,id?}, … ], meta: {items, ok, failed, duration_ms, trace_id} }`,
  order-preserving. Each item MAY carry an optional client-supplied `id`, echoed on its result for
  correlation and subset-retry (positional order is still guaranteed).
- Items are **independent**: no atomicity, no rollback of egress side effects, no cross-item
  ordering guarantee during execution (results array order ≠ execution order). No
  `sequential` option — ordered multi-step flows belong in one script (one execution) or in
  client-side sequential calls (decision recorded in design.md).
- Each item passes the existing per-request machinery individually: bulkhead admission,
  per-partition fairness, per-tenant quota debit, one usage event + one audit event per item,
  per-item validation and size limits.
- Batch-level caps enforced before any admission: `max_batch_items` and a combined-input-bytes
  limit; oversize batches rejected whole with a request-category error. A total-response-bytes cap
  bounds output so a batch cannot produce an unbounded response (offending items truncated to a
  size-limit error envelope). A single batch's intra-batch concurrency is bounded by the submitting
  partition's fair share of the runtime pool — it can never monopolize the pool.
- `LogicHost` is unchanged — the endpoint is a fan-out in the HTTP handler over the existing
  `run(Invocation)` port.
- Sequenced after `composable-capability-core`: per-item `meta` uses the new `meta.io` shape.

## Capabilities

### New Capabilities
- `batch-execution`: the batch request/response contract — item independence, order-preserving
  results, per-item envelopes/limits/admission/accounting, batch-level caps and validation.

### Modified Capabilities
- `execution`: the "Single execution endpoint" requirement changes — `/execute` is no longer
  the sole execution endpoint; `/batch` is added with per-item envelope semantics
  defined by `batch-execution`.

## Impact

- **Code**: `runlet/src/handler.rs` (+ router in `main.rs`), config (`max_batch_items`,
  combined-input-bytes cap, total-response-bytes cap), events/quota/authz touch only in that they
  are invoked per item.
- **Unchanged**: runlet-core, runlet-wire, fabricd, the single-execute path.
- **Tests/docs**: integration suite batch section (parallelism, partial failure, caps,
  quota/events accounting), README endpoint docs, `container/types.d.ts` batch envelope.
