## ADDED Requirements

### Requirement: Optional phased lifecycle

The `/batch` request SHALL accept three optional fields in addition to `items`:
`before` and `after` — each an invocation with the single-execute shape (`script` XOR `key`,
optional `context`, optional `config`) — and `shared`, a JSON object of read-only seed data.
When none are present, the batch SHALL behave exactly as it does today (backward compatible).
When present, execution SHALL proceed in three ordered phases: **`before`**, then the existing
concurrent **items** fan-out, then **`after`**.

#### Scenario: Batch without lifecycle fields is unchanged

- **WHEN** a batch body contains only `items` (no `before`, `after`, or `shared`)
- **THEN** it executes and responds identically to the pre-change behavior, with no `summary`

#### Scenario: Phase ordering

- **WHEN** a batch supplies `before`, `items`, and `after`
- **THEN** `before` completes before any item begins, every item completes before `after` begins, and the items still fan out concurrently among themselves

### Requirement: Immutable shared context

When `before` and/or `shared` are supplied, the system SHALL construct a shared context from the
`shared` seed object merged with `before`'s returned `data`, and SHALL expose it **read-only** to
every item and to `after`. Items SHALL NOT be able to mutate the shared context or observe writes
from sibling items through it; the context is fixed once `before` completes and is identical for
every item.

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
order-preserving `results` array as input. On success, `after`'s returned `data` SHALL be surfaced
as a batch-level `summary`. If `after` fails, the batch SHALL still respond HTTP 200 with the
per-item `results` intact and SHALL surface the failure as `meta.summary_error` rather than failing
the batch envelope.

#### Scenario: after produces the batch summary

- **WHEN** `after` runs and returns a reduced value over the `results`
- **THEN** the batch response carries that value as `summary` alongside the per-item `results`

#### Scenario: after failure does not fail successful items

- **WHEN** every item succeeds but `after` throws
- **THEN** the response is HTTP 200 with all per-item `results` present and a `meta.summary_error`, and no per-item result is altered

### Requirement: Lifecycle invocations pass the per-invocation gates

`before` and `after` SHALL each be treated as full invocations subject to the same per-invocation
machinery as an item (input/size validation, identity, per-tenant quota debit, capability
authorization, and — when events are enabled — usage/audit events); a lifecycle phase SHALL NOT be
a cheaper unit of admission, quota, or billing than an item. I/O in `before`/`after` SHALL be gated
by the execution profile exactly as it is for an item.

#### Scenario: Lifecycle phases debit quota

- **WHEN** a batch runs `before` and `after` in addition to its items
- **THEN** quota is debited for the `before` and `after` invocations as well as each item, so a batch with lifecycle phases is never cheaper than the equivalent count of single requests

#### Scenario: Deterministic profile denies I/O in lifecycle phases

- **WHEN** a batch runs under the deterministic profile and its `before` attempts an I/O capability
- **THEN** the I/O is denied exactly as it would be for a deterministic-profile item
