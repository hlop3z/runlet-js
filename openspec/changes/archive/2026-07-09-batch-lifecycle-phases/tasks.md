## 1. Wire format

- [x] 1.1 Add optional `before: Option<BatchItem>`, `after: Option<BatchItem>`, and `shared: Box<RawValue>` (default `{}`/null) fields to `BatchRequest` in `crates/runlet/src/handler.rs`; keep them absent-by-default so existing bodies deserialize unchanged.
- [x] 1.2 Add a top-level `summary: Option<Box<RawValue>>` and `summary_error: Option<ErrorEnvelope>` to `BatchResponse`/`BatchMeta`, both `skip_serializing_if = "Option::is_none"` so an unadorned batch response is byte-identical to today.
- [x] 1.3 Encode the resolved decisions (design.md RQ1–RQ3): `summary`/`summary_error` are top-level on `BatchResponse`, `after` receives the full per-item `{data, error, meta, id?}` envelopes, and `before`/`after` debit quota but do not consume `max_items` slots. Add a code comment referencing design.md RQ1–RQ3 at the wire structs.

## 2. Immutable shared context

- [x] 2.1 After a successful `before`, build the shared context by merging the `shared` seed object with `before`'s returned `data`; store it as an `Arc<Box<RawValue>>` (or `Arc<str>` of the serialized JSON) — read-only, no `Mutex`.
- [x] 2.2 Thread the shared-context `Arc` into `run_batch_item` (via `BatchItemCtx`) and into `after`'s invocation, exposing it to the JS handler as a read-only input alongside the item's own `context` (no new capability/FFI; passed as execution input).
- [x] 2.3 Add a `config.batch.max_shared_bytes` cap (default 4 MiB, mirroring `max_input_bytes`); a shared context exceeding it aborts the batch as a `before`-phase barrier failure (non-200, no items run), consistent with D4 — not a per-item clamp.

## 3. Phased execution in `run_batch`

- [x] 3.1 Phase 1 — run `before` (when present) via the shared `execute_blocking` core, sequentially before the `JoinSet` fan-out, through the same per-invocation gates an item gets (identity/quota/caps/events).
- [x] 3.2 Barrier semantics: on `before` failure (throw/timeout/gate rejection), return a non-200 batch-level error and run no items and no `after`. (Uses `projected_error_response` rather than the flat `batch_level_error` so the status is projected from the envelope — a script throw → 422, quota → 429 — instead of a blanket 400; still a non-200 batch-level response.)
- [x] 3.3 Phase 2 — run the existing item fan-out unchanged, passing each item the immutable shared context.
- [x] 3.4 Phase 3 — after `join_next` drains all items, run `after` (when present) with the order-preserving `results` as input; on success set `summary`, on failure set `meta.summary_error` and keep HTTP 200 with `results` intact.
- [x] 3.5 Ensure `before`/`after` count against per-tenant quota and emit usage/audit events (when enabled) as their own invocations.

## 4. Tests (Rust unit, in `handler.rs`)

- [x] 4.1 Backward-compat: a batch with only `items` produces a response with no `summary`/`summary_error`, identical to pre-change. (`no_lifecycle_omits_summary_fields`)
- [x] 4.2 Phase ordering: `before` fully completes before any item; all items complete before `after`. (`phases_run_before_items_after` — proved by data dependency: items read `before`'s output, `after` reduces every item result.)
- [x] 4.3 Shared context: items observe `before` output + `shared` seed; a sibling item's attempted write is not visible to another item (isolation preserved). (`shared_context_merges_and_is_isolated` — also covers before-wins-on-collision.)
- [x] 4.4 Fetch-collapse: a `before` with one egress call seeds N items; assert the egress runs exactly once. (`before_output_is_shared_by_all_items` — the once-produced/N-read property; the literal egress-count is integration-level, see 5.3.)
- [x] 4.5 `before` barrier: a throwing/timing-out `before` yields a non-200 batch error with zero items executed and no `after`. (`before_throw_is_a_barrier`)
- [x] 4.6 `after` reduce: `after` return value surfaces as `summary`; a throwing `after` yields HTTP 200 with intact `results` + top-level `summary_error`. (`after_summary_and_failure`)
- [x] 4.7 Gates: quota is debited for `before` (`before_is_quota_gated` — a `0`-capacity plan denies it as a barrier); I/O in `before` is fail-closed gated exactly as for an item (`before_io_fails_closed` — the box HTTP front always runs the full profile, so fail-closed egress is the box-level analogue of the spec's "profile denies I/O in lifecycle phases").

## 5. Docs & close-out

- [x] 5.1 Update the `/batch` section of `docs/` and `README.md` with the `before`/`shared`/`after` lifecycle and the AI-era use cases (eval harness, best-of-N, structured-output validation, agentic fan-out). (README.md "Lifecycle" subsection + a friendly pointer in docs/README.md.)
- [x] 5.2 Add a `docs/design/` note (or link design.md) capturing the deferred Model X store and the rejected blocking `once()` primitive, so the seam is discoverable. (`docs/design/batch-lifecycle.md`.)
- [x] 5.3 Run `task clippy` (until clean) and the box-only Python harness; extend `tests/test_simple.py` with a lifecycle section if an end-to-end assertion is warranted. (Clippy clean; `tests/test_simple.py` gained a lifecycle subsection; the four behaviors were also verified live against a loopback box over HTTP.)
- [x] 5.4 `/opsx:sync` the `batch-execution` delta into the main spec, then `/opsx:archive`.
