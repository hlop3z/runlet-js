# The batch lifecycle — `before` / `shared` / `after`

`POST /batch` fans out N **independent** items. Two AI-era shapes need work *around* that fan-out,
not inside it: a **shared expensive fetch** every item needs (an LLM/embedding/rerank call, a
rubric, a schema) and a **cross-item reduce** (an eval accuracy report, a best-of-N vote, a
structured-output validation summary). The lifecycle closes both gaps with an optional one-time
setup phase and a reduce phase, and does so with **zero new concurrency failure modes**.

```
before  (once, alone)  ── builds the immutable shared context ──▶
items   (concurrent fan-out; each reads ctx.shared read-only)  ──▶
after   (once, alone)  ── reduces ctx.results into the summary
```

See the user-facing contract in [`README.md` → Batch execution](../../README.md#batch-execution).

## Coordinate by phasing, not by locking ("Model Y")

`before` and `after` run **alone** — nothing else executes during them — so they need no locks, no
blocking, and cannot deadlock. The shared context is built once by `before` and is thereafter
**immutable** across the concurrent fan-out: an `Arc<str>` shared read-only across items, never a
`Mutex`. Each item parses its own copy, so a write by one item is invisible to another. The only
concurrent phase touches no shared mutable state — the box's "every request independent, nothing
waits on anything" invariant holds exactly.

The two phases are ordinary invocations: they pass the **same per-invocation gates an item gets**
(size, authz, per-tenant quota debit, capability profile, events) and are billed as their own
invocations, so a batch with a lifecycle is never a cheaper unit of work than the equivalent single
requests. They do **not** consume `batch.max_items` slots (they are fixed per-batch overhead), and
the shared context is bounded by `batch.max_shared_bytes`. `before` is a **barrier** (any failure
aborts the whole batch non-200, no item runs); `after` is **best-effort** (a failure keeps the 200
with `results` intact and reports `summary_error`) — a failed reducer must not discard the
successfully-mapped rows.

## The seam: what was deliberately *not* built

The engine is **synchronous**: the wall-clock deadline is a QuickJS interrupt that only fires while
the bytecode interpreter is looping, so a blocking Rust FFI call is uninterruptible by the timeout.
That is why any *blocking coordination* primitive is dangerous here, and why the lifecycle covers
the real needs (fetch-collapse via `before`, aggregation via `after`) without one.

- **Rejected outright — a blocking `once(key, fn)` / single-flight at item runtime.** Best DX for
  fetch-collapse, but "block the losers until the leader finishes" means parking a `spawn_blocking`
  thread **while it holds a pooled runtime**, with the wall-clock interrupt unable to fire during
  the park. It reintroduces lock-ordering / self-deadlock (`once("x")` nesting `once("y")` vs. the
  reverse) and runtime-pool pressure (parked-but-holding losers starve other tenants onto the cold
  `create_runtime` fallback) — net-new failure modes the box has never had. Phasing gets the same
  fetch-collapse with none of it. **Do not revive this**, even if the store below is later added.

- **Deferred, recorded — the mutable atomic store ("Model X").** A shared `Arc<Mutex<map>>` with
  wait-free ops (`get`/`set`/`has`/`set_default`/`add`/`push`/`snapshot`) would let items coordinate
  **mid-flight** (item B skips work item A already did). It is invariant-*preserving* in itself
  (single lock hop, no cross-item waiting) but is out of scope: Model Y already covers fetch-collapse
  and every end-of-batch aggregation, and mid-flight mutual-exclusion is the *only* thing it cannot
  do — with no concrete case for it yet. If one arrives, add the store as an **additive** extension
  to this same lifecycle.

The full decision record (D1–D5, RQ1–RQ3) lives in the change's `design.md`
(`openspec/changes/archive/…-batch-lifecycle-phases/design.md`).
