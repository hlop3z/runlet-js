# Design: batch-execute-endpoint

## Context

The per-request machinery this endpoint needs already exists and is battle-tested: Tier-1
bulkhead admission and Tier-5 per-partition fairness (`config.rs`:
`max_concurrent_executions` / `max_concurrent_per_partition`), per-tenant quota
(`quota.rs`), per-execution usage/audit events (`events.rs`), and the callable
`LogicHost::run(Invocation) → Outcome` port. Batching is a fan-out over that port in the
HTTP layer. Sequenced after `composable-capability-core` so per-item `meta` is born with the
`meta.io` shape.

## Goals / Non-Goals

**Goals**
- Amortize HTTP overhead for independent, high-volume executions while keeping per-item
  isolation, timeouts, envelopes, and billing dims.
- Zero new fairness/quota machinery — a batch is exactly N single requests in cost and
  admission.

**Non-Goals**
- Atomicity/transactionality across items (egress side effects cannot roll back).
- Sequential/ordered execution mode (D1).
- Streaming/chunked responses; changes to `LogicHost` or runlet-core.

## Decisions

### D1 — No `sequential` option
Items are independent and concurrently schedulable; the response array preserves *result*
order only. Verified against `docs/use-cases.md` before deciding: every ordered flow there
(inter-system sync, CI/CD steps, multi-step ETL) is steps *within one script* — a single
execution already serves it with shared state and strict order. No use case needs per-item
isolation *and* cross-item ordering; client-side sequential calls cover the remainder.
Offering the flag would blur the "items are independent" invariant that makes the
no-atomicity contract safe to state. *Alternative*: `"sequential": true` — rejected as
attractive-nuisance surface.

### D2 — Thin fan-out in the handler; per-item admission, bounded by the partition's fair share
The endpoint validates batch-level caps, then treats each item exactly as `/execute` does:
validate → bulkhead admit → `spawn_blocking` → `host.run`. Concurrency emerges from the
existing permits (min of batch size, free bulkhead slots, per-partition cap) — a batch
cannot preempt other tenants. The invariant that makes this safe (and that a test must assert):
a single batch's items are subject to the **same per-partition concurrency ceiling** as
independent requests, so one batch can occupy at most its partition's share of the runtime pool,
never the whole pool. This bounds worst-case connection hold time to
(items ÷ partition ceiling) × per-item wall-clock timeout — the concrete reason OpenAI/Shopify/
Stripe refuse *large* synchronous batches and we keep `max_batch_items` small (D3). *Alternative*:
batch-level admission (one permit per batch) — rejected: makes a batch cheaper than N requests and
starves fairness.

### D3 — Batch caps before any admission
`max_batch_items` (config; default modest, e.g. 25) and a combined-bytes cap reuse the
existing byte-size validation approach; empty and oversize batches are rejected whole with a
request-category error. Per-item script/context size limits still apply individually.

### D4 — HTTP semantics: 200 with per-item errors
A batch that was admitted returns HTTP 200 with per-item envelopes (partial failure is
normal operation, expressed in `results[i].error` + batch `meta.ok/failed`). Only
batch-level rejections (malformed body, caps, auth) use non-200. *Alternative*: 207
Multi-Status — rejected: WebDAV-ism; clients already branch on the envelope.

### D5 — Accounting is strictly per item
Quota debit, usage event, audit event, span: one per item, same code paths as single
execute; the batch adds only a parent trace/span and a batch `trace_id` in batch `meta`.
This is the invariant that prevents batching from becoming a billing/quota bypass. **This
extends to authorization**: trusted-mode capability/entitlement gating (`authz.rs`) and quota
(`quota.rs`) are evaluated per item, never once for the batch — the documented GraphQL-batch-attack
failure is exactly a system that counted the HTTP request instead of the operations inside it
(batched OTP/2FA brute force). Everything that debits or gates counts N, not 1.

### D6 — Bound response size, not just request size
`max_batch_items` + a combined-input-bytes cap (D3) bound the *request*; nothing there bounds the
*response*. N items each returning a large `data`/`meta` blob is a documented amplification/memory
vector. Add a total-response-bytes cap (and/or per-item output cap); on exceed, truncate the item to
a classified size-limit error envelope (default) — the per-item envelope makes this expressible
without failing the whole batch. *Alternative*: bound only inputs — rejected; output size is
independent of input size for arbitrary JS and is the actual server-memory risk.

### D7 — Optional client-supplied per-item `id`, echoed back (additive now)
Order-by-index is the correct base contract, but positional-only correlation breaks the moment a
client retries a subset, filters, or we later add streaming/async out-of-order completion. Accepting
an optional `id` per item and echoing it is cheap now and expensive to retrofit once clients depend
on position. It also becomes the join key for per-item usage/audit events (D5) and the natural unit
for a future per-item idempotency key. The retry contract we document: on partial failure, resubmit
only the items whose envelope carried an error. *Alternative*: index-only — rejected as a
known-regret once subset-retry appears.

### Decision: Batch fan-out / response-size / per-item idempotency — Build (reuse existing machinery)

- **Status**: approved
- **Why**: the fan-out runs over the in-house `LogicHost::run` port and reuses the existing bulkhead,
  per-partition fairness, quota, events, and byte-size validation; no mature library batches over a
  private execution port, and v1 idempotency is echoing a client-supplied item `id` (no cached-outcome
  store yet). Adopting anything would wrap machinery we already own.
- **Considered**: adopt a batch/idempotency framework — none fit fan-out over an internal port; a
  Stripe-style cached-outcome idempotency store is a future add, not v1.
- **Isolation**: confined to `runlet/src/handler.rs` (+ the router in `main.rs`); `LogicHost` and
  `runlet-core` are untouched.

## Risks / Trade-offs

- [A huge batch ties up the caller's whole fairness allowance] → intended: it queues within
  the tenant's own cap; document that latency-sensitive callers should keep batches small.
- [Combined response size can be large] → bounded by `max_batch_items` × existing per-item
  response behavior; document; no streaming in v1.
- [Quota race at the boundary (N+1 items, N remaining)] → per-item debit uses the existing
  atomic quota path; excess items fail with the standard quota error (spec scenario).
- [Trusted-mode headers apply batch-wide but items could be assumed per-item] → identity is
  connection/request-level by design; all items share the request's tenant — document.

## Migration Plan

1. Additive endpoint; no existing behavior changes. Deploy normally.
2. Rollback: remove the route; `/execute` untouched.

## Open Questions

- Default `max_batch_items` (25 vs 100) — pick after a runlet-bench measurement of pool
  saturation behavior under batch load.
- Whether batch `meta` should echo per-item duration percentiles (lean: no; `results[i].meta`
  already carries per-item timing).
