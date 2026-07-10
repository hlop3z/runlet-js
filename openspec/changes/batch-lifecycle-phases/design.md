## Context

`POST /batch` (`crates/runlet/src/handler.rs`) is a thin fan-out over the single-`/execute` path:
`run_batch` resolves auth + identity once, validates batch-level caps, then spawns each item
through `run_batch_item` (the shared `execute_blocking` core plus the identical per-item gates) on a
`tokio::JoinSet`, bounded to the partition's fair share, and assembles order-preserving
`{data, error, meta, id?}` envelopes. Items are already **pure and isolated**: each acquires a
pooled QuickJS runtime and a fresh `Context`, so global scope never leaks between items.

Two AI-era shapes are currently impossible: a **shared expensive fetch** (an LLM/embedding/rerank
call, a rubric, a schema) must be repeated per item, and a **cross-item reduce** (eval accuracy
report, best-of-N vote, validation summary) must be done client-side. This change adds an optional
`before`/`after` lifecycle to close both gaps.

The dominant constraint is the execution model: **the engine is synchronous.** The wall-clock
deadline is a QuickJS interrupt handler (`engine.rs::setup_timeout`) that only fires while the
bytecode interpreter is looping. While a Rust FFI call (`__io`) blocks, the interpreter is not
looping, so the deadline interrupt cannot fire — a blocking FFI call is uninterruptible by the
timeout. This is why any *blocking coordination* primitive is dangerous here (see Decisions).

## Goals / Non-Goals

**Goals:**

- Run one-time shared setup (`before`) whose output is an immutable context every item reads —
  collapsing an N-fold shared fetch to a single call.
- Run one reduce step (`after`) over the results array, surfaced as a batch-level `summary`.
- Add **zero** new concurrency failure modes: preserve the box's "every request independent, nothing
  waits on anything" invariant exactly.
- Keep the change confined to `crates/runlet/src/handler.rs`; leave `runlet-core` and `/execute`
  untouched.

**Non-Goals:**

- No shared **mutable** store and no atomic ops (`add`/`push`/`set_default`) during the fan-out.
- No blocking single-flight / `once()` primitive.
- No mid-flight cross-item coordination (item B skipping work item A already did). Deferred (below).
- No sequential/transactional item execution mode.

## Decisions

### D1: Coordinate by phasing, not by locking ("Model Y")

`before` and `after` run **alone** — nothing else executes during them — so they need no locks, no
blocking, and cannot deadlock. The shared context is built once by `before` and is thereafter
**immutable** during the concurrent fan-out, so it is an `Arc<...>` shared read-only across items,
never a `Mutex`. Items remain pure functions of `(their input, the shared context)`. This keeps the
concurrency/isolation invariants identical to today: the only concurrent phase touches no shared
mutable state.

- **Alternatives considered — rejected:**
  - **Blocking `once(key, fn)` / single-flight at item runtime.** Best DX for fetch-collapse, but in
    a synchronous engine "block a loser until the leader finishes" means parking an OS
    (`spawn_blocking`) thread **while it holds a pooled runtime**, with the wall-clock interrupt
    unable to fire during the park. It reintroduces lock-ordering / self-deadlock (`once("x")`
    nesting `once("y")` vs. the reverse) and runtime-pool pressure (parked-but-holding losers starve
    other tenants onto the cold `create_runtime` fallback) — net-new failure modes the box has never
    had. Phasing gets the same fetch-collapse with none of it.
  - **Non-blocking leader election (`set_default`-style claim).** No blocking, but it dedupes the
    *stored value*, not the *fetch* — up to N items still fetch before one wins, defeating the point.

### D2: Reuse the existing per-item machinery for `before`/`after`

`before` and `after` are just invocations. Implement them by calling the same `execute_blocking`
core `run_batch_item` uses, run sequentially outside the `JoinSet` (before it, and after
`join_next` drains). They pass the identical per-invocation gates (identity/quota/caps/events) so
they are never a cheaper unit of work than an item, satisfying the bounded-consumption invariant.
`before`/`after` accept the same `{script}` XOR `{key}` shape, so a reducer can be a registered
script.

- **Alternative — rejected:** a bespoke lightweight path for lifecycle phases. More code, and it
  would bypass the gates that keep a batch from being a cheaper unit than N requests.

### D3: `before` is a barrier; `after` is best-effort

`before` failure aborts the whole batch non-200 with no items run — items depend on the seed, so
partial execution would be meaningless. `after` failure does **not** fail the batch: items already
succeeded and their `results` are the primary product, so an `after` error is surfaced as
`meta.summary_error` on a 200 response. This mirrors a map-reduce where a failed reducer shouldn't
discard the successfully-mapped rows.

### D4: Shared context is passed as immutable execution input, not a new capability

The shared context reaches items as read-only input on the invocation (alongside the item's own
`context`), not as an injected mutable global or FFI. No `runlet-core` capability, engine, or
profile change is required; the box stays kind-blind and the item contract stays "pure handler." A
byte cap on the shared context (config, mirroring `config.batch.*` and the `OutputTooLarge`
precedent) bounds its size; `before`'s own `max_output_size` already bounds what it can return.

### D5 (DEFERRED — recorded, not built): the mutable atomic store ("Model X")

A shared `Arc<Mutex<map>>` with wait-free atomic ops (`get`/`set`/`has`/`set_default`/`add`/`push`/
`remove`/`snapshot`) would let items coordinate **mid-flight** (item B skips work item A already
did). It is invariant-*preserving* in itself (single lock hop, no cross-item waiting) but is
deliberately **out of scope** here: Model Y already covers fetch-collapse (`before`) and every
end-of-batch aggregation (`after` reducing `results`); mid-flight mutual-exclusion is the *only*
thing it cannot do, and there is no concrete case for it yet. If one arrives, add the store as an
additive extension to this same lifecycle. The blocking `once()` primitive is rejected outright per
D1 and should not be revived even if the store is later added.

## Risks / Trade-offs

- **No automatic fetch dedup for values that differ per item** → by design: `before` only collapses
  a fetch that is *shared* by the whole batch. Per-item fetches stay per-item; that is the correct
  boundary.
- **Two extra sequential hops (`before`, `after`) add latency** → bounded and opt-in: a batch pays
  for them only when it supplies them, and each is one invocation, not per-item.
- **`after` holds the full `results` array in memory to reduce it** → already true (the batch
  buffers all results for the response); the existing total-response-bytes cap continues to bound
  it, and `after`'s reduced `summary` is separately bounded by its own `max_output_size`.
- **A large shared context is copied read-only to every item** → it is an `Arc`, shared not cloned;
  the byte cap (D4) bounds worst case.
- **Scope discipline** → this is *capability* growth, not *risk* growth: it adds power
  (setup/reduce) without adding any new sandbox failure mode. The tempting-but-dangerous
  primitives (`once`, mutable store) are explicitly deferred/rejected with reasons, so the seam is
  documented rather than rediscovered.

## Resolved Questions

The three open questions were resolved (session 2026-07-09) as follows; they now bind the
implementation:

### RQ1: `summary` is a top-level field on `BatchResponse`

`summary` sits at the top level, symmetric with the top-level `results`; `summary_error` sits next
to it (also top-level). Both are `skip_serializing_if = "Option::is_none"`, so an unadorned batch
response is byte-identical to today. (Rejected: nesting under `meta` — the summary is a primary
product of the batch, peer to `results`, not metadata about it.)

### RQ2: `after` receives the full per-item envelopes

`after`'s `results` input is the order-preserving array of full `{data, error, meta, id?}`
envelopes — the same shape the batch returns — so a reducer can compute `ok`/`failed` breakdowns and
read per-item errors and meta. (Rejected: passing only `data`, which would blind the reducer to
per-item failures — the exact information an eval/best-of-N reducer needs.)

### RQ3: lifecycle phases debit quota but do NOT consume `max_items` slots

`before`/`after` are full invocations for **admission, quota, caps, and events** (per the
"lifecycle invocations pass the per-invocation gates" requirement — a batch with lifecycle is never
cheaper than the equivalent single requests), but they do **not** count against `config.batch.max_items`.
That cap governs fan-out width; `before`/`after` are fixed per-batch overhead (≤2), so a full batch
of `max_items` items plus a `before`/`after` is still admitted. The shared-context byte cap is a new
`config.batch.max_shared_bytes`, default **4 MiB** (mirroring `max_input_bytes`, since the shared
context is input-shaped data handed read-only to every item); an over-cap shared context aborts the
batch as a `before`-phase barrier failure, consistent with D4.
