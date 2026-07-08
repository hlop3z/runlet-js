# batch-execution Specification

## Purpose

The `/batch` endpoint runs multiple client-supplied executions in one request, returning an
order-preserving `results` array where each entry is the same `{data, error, meta}` envelope a
single `/execute` produces. Items execute independently (no atomicity), each passing the full
per-request admission/limits/accounting machinery, under batch-level and per-batch resource
caps so a batch is never a cheaper or unbounded unit of work than N single requests.

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
