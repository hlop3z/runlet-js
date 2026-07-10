## Why

`POST /batch` today fans out N fully independent items — there is no way to run one-time
shared setup before the fan-out or to reduce the results into a single answer after it. Two
patterns that recur in AI-era workloads are therefore impossible without duplicating work: a
**shared, expensive fetch** every item needs (an LLM/embedding/rerank call, a rubric, a schema)
must be repeated per item, and a **cross-item aggregation** (an eval accuracy report, a
best-of-N vote, a validation summary) has to be done by the client after the fact. Adding a
`before`/`after` lifecycle closes both gaps with **zero new concurrency risk**, because the
phases run when nothing else runs — coordination by phasing, not by locking.

## What Changes

- Extend the `/batch` request with three optional fields:
  - **`before`** — a `{script}` XOR `{key}` invocation that runs **once, alone, before any item**.
    Its return value plus an optional **`shared`** seed object become an **immutable shared
    context** handed to every item.
  - **`shared`** — a JSON object merged into the shared context as read-only seed data (constants
    that need no fetch).
  - **`after`** — a `{script}` XOR `{key}` invocation that runs **once, alone, after all items
    complete**. It receives the `results` array; its return value becomes a batch-level `summary`.
- Execution becomes three phases: **`before` → items (existing concurrent fan-out) → `after`**.
  Items stay **pure** — they read the immutable shared context and return their own data; there
  is **no shared mutable state and no cross-item coordination during fan-out**.
- Lifecycle error semantics:
  - `before` throws ⇒ **barrier failure**: the whole batch aborts non-200 and **no item runs**.
  - an item throws ⇒ per-item error, batch continues (unchanged behavior).
  - `after` throws ⇒ items already succeeded ⇒ HTTP 200 with `meta.summary_error`; the batch
    envelope is **not** failed.
  - `after` returns ⇒ value surfaced as the batch-level `summary`.
- `before`/`after` are full invocations subject to the **same per-invocation gates** an item gets
  (identity/quota/caps; batch-level auth already applies) and count against quota; I/O in them is
  gated by `Profile::Full` exactly like an item.
- **No** shared mutable store, atomic ops, or blocking single-flight primitive is introduced.
  End-of-batch aggregation is done by reducing the `results` array in `after`.

## Capabilities

### New Capabilities
<!-- none — this extends an existing capability -->

### Modified Capabilities
- `batch-execution`: adds the optional `before`/`shared`/`after` phased lifecycle to the existing
  fan-out — an immutable shared context produced by `before`, consumed read-only by items, and a
  reduce step in `after` whose output is the batch `summary`. The existing item-independence,
  per-item admission/limits/accounting, bounded-consumption, and batch-level-caps requirements are
  unchanged and continue to apply to `before`/`after` as invocations.

## Impact

- **Code:** `crates/runlet/src/handler.rs` only — the `/batch` request struct (`BatchRequest`
  gains `before`/`shared`/`after`), `run_batch` (phased into before → fan-out → after, reusing the
  existing `run_batch_item`/`execute_blocking` core), and `BatchResponse`/`BatchMeta` (add
  `summary` / `summary_error`). The existing per-item path is unchanged.
- **`runlet-core` is untouched** — no engine, pool, capability, or profile change; items remain
  pure and the concurrency/isolation invariants hold exactly (before/after run alone).
- **`POST /execute` is untouched.**
- **API:** additive, backward-compatible — a batch body without `before`/`shared`/`after` behaves
  identically to today.
- **Config:** may add optional caps for the shared-context byte size and `before`/`after`
  presence, alongside the existing `config.batch.*` limits.
