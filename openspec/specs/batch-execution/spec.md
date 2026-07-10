# batch-execution Specification

## Purpose

The `/batch` endpoint runs multiple client-supplied executions in one request, returning an
order-preserving `results` array where each entry is the same `{data, error, meta}` envelope a
single `/execute` produces. Items execute independently (no atomicity), each passing the full
per-request admission/limits/accounting machinery, under batch-level and per-batch resource
caps so a batch is never a cheaper or unbounded unit of work than N single requests. An optional
`before`/`shared`/`after` lifecycle adds a one-time setup phase whose output is an immutable
shared context every item reads, and a one-time reduce phase over the results — coordination by
phasing (the phases run alone) rather than locking, so items stay pure and no new concurrency
failure mode is introduced.

## Requirements

### Requirement: Batch endpoint and envelope

The system SHALL expose `POST /batch` accepting `{ items: [...] }` where each item
has the single-execute shape (`script` XOR `key`, optional `context`, optional `config`),
and SHALL respond with `{ results: [...], meta }` where `results[i]` is the full
`{data, error, meta}` envelope for `items[i]` (order-preserving) and the batch `meta`
carries `items`, `ok`, `failed`, `duration_ms`, and a batch `trace_id`. Each item MAY carry an
optional client-supplied `id`; when present it SHALL be echoed on the corresponding `results[i]`
entry, in addition to (not instead of) positional ordering.

#### Scenario: Order-preserving results

- **WHEN** a batch submits items A, B, C
- **THEN** `results[0..2]` correspond to A, B, C regardless of the order they finished executing

#### Scenario: Client item id is echoed for correlation

- **WHEN** a batch item carries a client-supplied `id`
- **THEN** its `results[i]` entry echoes that `id`, so a client can correlate and safely resubmit only failed items without relying on array position alone

#### Scenario: Per-item envelope is the single-execute envelope

- **WHEN** any batch item completes (success or failure)
- **THEN** its `results[i]` entry has exactly the keys `data`, `error`, and `meta`, with the same semantics as a `/execute` response

### Requirement: Item independence — no atomicity

Batch items SHALL execute independently: one item's failure SHALL NOT affect any other item,
no egress side effect is rolled back, and the system SHALL make no cross-item ordering or
transactionality guarantee. The batch endpoint provides no sequential-execution mode.

#### Scenario: Partial failure

- **WHEN** a batch contains one item that times out and others that succeed
- **THEN** the timed-out item's entry carries error code `TIMEOUT` while every other entry succeeds, and batch `meta` reports `ok`/`failed` counts accordingly

#### Scenario: Isolation between items

- **WHEN** one batch item mutates global scope
- **THEN** every other item observes a clean global scope (fresh context per item)

### Requirement: Per-item admission, limits, and accounting

Each batch item SHALL individually pass the same per-request machinery as a single execute:
input validation and size limits, bulkhead admission, per-partition fairness, per-tenant
quota debit, and (when events are enabled) one usage event and one audit event per item. A
batch SHALL NOT constitute a cheaper unit of admission, quota, or billing than N single
requests.

#### Scenario: Fairness holds under a large batch

- **WHEN** a tenant submits a batch larger than its per-partition concurrency cap while another tenant runs requests
- **THEN** the batch's items queue through the tenant's own fairness allowance and the other tenant's latency is unaffected

#### Scenario: Quota debits per item

- **WHEN** a tenant with N remaining quota executions submits a batch of N+1 items
- **THEN** exactly N items can be admitted and the excess item fails with the same quota-exceeded error a single request would receive

#### Scenario: One usage event per executed item

- **WHEN** events are enabled and a batch of 3 items executes
- **THEN** 3 usage events are emitted, each keyed to the tenant with that item's billing dims

#### Scenario: Authorization is evaluated per item

- **WHEN** trusted mode is active and a batch's items invoke capabilities gated by roles/entitlements
- **THEN** the capability/quota authorization is evaluated for every item (not once for the batch), so a batch cannot be used to smuggle an operation past a per-request authz gate — the GraphQL-batch-attack failure mode

### Requirement: Bounded batch resource consumption

A single batch SHALL NOT be able to exhaust shared server resources on behalf of one tenant. The
system SHALL cap total response bytes (and/or per-item output bytes) so a batch cannot produce an
unbounded response, and the intra-batch concurrency of one batch SHALL be bounded by the submitting
partition's fair share of the runtime pool — a batch may occupy at most its partition's concurrency
ceiling, never the whole pool.

#### Scenario: Response-size cap

- **WHEN** a batch's accumulated item outputs would exceed the configured total-response-bytes cap
- **THEN** the offending item(s) are truncated to a classified size-limit error envelope (or the batch is rejected, per config), rather than the server buffering an unbounded response

#### Scenario: A single batch cannot monopolize the runtime pool

- **WHEN** a batch with more items than the partition's concurrency cap is admitted
- **THEN** at most the partition's ceiling of items execute concurrently and the remainder queue, so other partitions retain their share of runtime slots and a batch's worst-case connection hold time is bounded by (items ÷ partition ceiling) × per-item wall-clock timeout

### Requirement: Batch-level caps

The system SHALL enforce a configurable maximum item count (`max_batch_items`) and a
combined-bytes limit on the batch body, rejecting an oversize batch whole with a
request-category error before any item is admitted or executed.

#### Scenario: Too many items

- **WHEN** a batch exceeds `max_batch_items`
- **THEN** the response is HTTP 400 with a request-category error and no item executes

#### Scenario: Empty batch

- **WHEN** a batch contains zero items
- **THEN** the response is HTTP 400 with a request-category error

#### Scenario: Malformed item fails only itself

- **WHEN** a batch within the caps contains one item with both `script` and `key`
- **THEN** that item's entry carries error code `SCRIPT_XOR_KEY` and the remaining items execute normally

### Requirement: Optional phased lifecycle

The `/batch` request SHALL accept three optional fields in addition to `items`:
`before` and `after` — each an invocation with the single-execute shape (`script` XOR `key`,
optional `context`, optional `config`) — and `shared`, a JSON object of read-only seed data.
When none are present, the batch SHALL behave exactly as it does today (backward compatible) and
its response SHALL carry no `summary`/`summary_error`. When present, execution SHALL proceed in
three ordered phases: **`before`**, then the existing concurrent **items** fan-out, then
**`after`**.

#### Scenario: Batch without lifecycle fields is unchanged

- **WHEN** a batch body contains only `items` (no `before`, `after`, or `shared`)
- **THEN** it executes and responds identically to the pre-lifecycle behavior, with no `summary` and no `summary_error`

#### Scenario: Phase ordering

- **WHEN** a batch supplies `before`, `items`, and `after`
- **THEN** `before` completes before any item begins, every item completes before `after` begins, and the items still fan out concurrently among themselves

### Requirement: Immutable shared context

When `before` and/or `shared` are supplied, the system SHALL construct a shared context from the
`shared` seed object merged with `before`'s returned `data` (with `before`'s data taking precedence
on key collisions), and SHALL expose it **read-only** to every item and to `after`. Items SHALL NOT
be able to mutate the shared context or observe writes from sibling items through it; the context is
fixed once `before` completes and is identical for every item. The serialized shared context SHALL be
bounded by a configurable cap (`max_shared_bytes`); exceeding it aborts the batch as a `before`-phase
barrier failure.

#### Scenario: Items read the shared context

- **WHEN** `before` returns data and each item reads the shared context
- **THEN** every item observes the same shared context value produced by `before` plus the `shared` seed

#### Scenario: Shared context is not a cross-item channel

- **WHEN** one item attempts to write to the shared context and another item reads it
- **THEN** the reader observes only the immutable value fixed at the end of `before` (no sibling write is visible), preserving item isolation

#### Scenario: One-time shared fetch is not duplicated

- **WHEN** `before` performs a single egress fetch and seeds its result into the shared context for a batch of N items
- **THEN** the fetch executes exactly once for the whole batch, not once per item

### Requirement: `before` is a barrier

The `before` phase SHALL act as a barrier: if `before` fails (throws, times out, or is rejected
by any per-invocation gate), the system SHALL abort the whole batch with a non-200 batch-level
error and SHALL NOT execute any item or `after`.

#### Scenario: before failure aborts the batch

- **WHEN** a batch's `before` throws or times out
- **THEN** the response is a non-200 batch-level error, no item executes, and `after` does not run

### Requirement: `after` reduces results into a summary

When `after` is supplied, it SHALL run once after all items complete and SHALL receive the
order-preserving `results` array — the full per-item `{data, error, meta}` envelopes — as input. On
success, `after`'s returned `data` SHALL be surfaced as a **top-level** batch `summary` (peer to
`results`). If `after` fails, the batch SHALL still respond HTTP 200 with the per-item `results`
intact and SHALL surface the failure as a **top-level** `summary_error` rather than failing the batch
envelope. Both `summary` and `summary_error` SHALL be omitted when `after` is absent.

#### Scenario: after produces the batch summary

- **WHEN** `after` runs and returns a reduced value over the `results`
- **THEN** the batch response carries that value as a top-level `summary` alongside the per-item `results`

#### Scenario: after reads full per-item envelopes

- **WHEN** `after` reduces a batch containing both succeeded and failed items
- **THEN** it can read each item's `data`, `error`, and `meta` (not just `data`), so it can compute ok/failed breakdowns and inspect per-item errors

#### Scenario: after failure does not fail successful items

- **WHEN** every item succeeds but `after` throws
- **THEN** the response is HTTP 200 with all per-item `results` present and a top-level `summary_error`, and no per-item result is altered

### Requirement: Lifecycle invocations pass the per-invocation gates

`before` and `after` SHALL each be treated as full invocations subject to the same per-invocation
machinery as an item (input/size validation, identity, per-tenant quota debit, capability
authorization, and — when events are enabled — usage/audit events); a lifecycle phase SHALL NOT be
a cheaper unit of admission, quota, or billing than an item. I/O in `before`/`after` SHALL be gated
by the execution profile exactly as it is for an item. Lifecycle phases SHALL NOT count against the
`max_batch_items` cap — that cap governs only the fan-out width.

#### Scenario: Lifecycle phases debit quota

- **WHEN** a batch runs `before` and `after` in addition to its items
- **THEN** quota is debited for the `before` and `after` invocations as well as each item, so a batch with lifecycle phases is never cheaper than the equivalent count of single requests

#### Scenario: Lifecycle phases do not consume item slots

- **WHEN** a batch supplies exactly `max_batch_items` items plus a `before` and an `after`
- **THEN** the batch is still admitted — the lifecycle phases are fixed per-batch overhead, not counted against `max_batch_items`

#### Scenario: I/O in a lifecycle phase is gated like an item

- **WHEN** a batch's `before` names an egress resource that would be denied to an item (e.g. no egress is available, or the execution profile forbids I/O)
- **THEN** the I/O is denied exactly as it would be for an item, and (being a `before` failure) it aborts the batch as a barrier
