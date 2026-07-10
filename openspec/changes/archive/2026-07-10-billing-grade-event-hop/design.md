## Context

`crates/runlet/src/events.rs` emits per-request `usage` and `audit` events (billing + compliance)
and diagnostic `log` events. The `Sink` port fronts a `LogSink` that hands events to bounded
`tokio::mpsc` channels drained by writer tasks that `println!` one JSON line per event to stdout. The
D4 split already gives usage/audit their **own** channel, isolated from the lossy `log` channel.

Today `Sink::record` uses `try_send`: on a full channel the usage/audit event is **silently dropped**
and a counter bumped. That is a revenue/compliance leak under load, and the in-channel backlog is
lost on a hard crash. The `Sink` trait, the per-event `event_id` dedup key, and the isolated channel
were all built as the seam for making this stream durable (see `tenant-metering` /  `tenant-audit`
specs and `docs/design/multitenant-trust.md`).

Emission call sites are `emit_executed` / `emit_denied` in `handler.rs`, both sync helpers invoked
from the **async** request handler *after* the `spawn_blocking` execution returns — so an async,
timeout-bounded enqueue is available without touching the blocking QuickJS path. The strict lint
gauntlet applies (no `unwrap`/`expect`/`panic`, no bare arithmetic, no `as`, `#[expect]` over
`#[allow]`); build/test are Docker-only.

The target deployment is the box behind a control plane with a log-shipping collector: the container
runtime persists the box's stdout to a node-local file, and a checkpointing collector tails it into
the control plane's billing ingest (deduplicating on `event_id`). That file **is** the durable
at-least-once outbox — the box does not need its own on-disk store in this model.

## Goals / Non-Goals

**Goals:**

- The box never *silently* drops a `usage` or `audit` event on its own hop — a momentary full channel
  waits briefly for capacity instead of dropping.
- A genuine drop (sustained saturation) is rare and surfaced as a loud SLO/alerting signal, not a
  quiet counter.
- The request path is never blocked unboundedly — waiting is capped by a small, configurable timeout.
- Graceful shutdown flushes the precious usage/audit channel before exit.
- `log` events stay best-effort/lossy on their separate channel (unchanged).
- The `Sink` port stays the seam so a durable, on-disk impl can drop in later with zero emission-site
  change.

**Non-Goals:**

- No on-box durable storage (no redb, no SQLite, no local WAL) in this change — documented as
  adopt-later.
- No exactly-once and no ordering guarantee — delivery stays at-least-once with `event_id` dedup
  downstream.
- No change to `record_log` (log) semantics, the event envelope/schema, or `runlet-core`.
- No change to what a collector/control plane does downstream (out of this repo, like `fabricd`).

## Decisions

### D1 — Bounded block-with-timeout for the precious channel; log stays fire-and-forget

The usage/audit enqueue becomes an async `tokio::mpsc::Sender::send_timeout(event, timeout).await`:
it waits for capacity up to a small configurable timeout and only drops-and-counts if still saturated
when the timeout expires. `record_log` keeps its sync `try_send` fire-and-forget path. Because
`emit_executed` / `emit_denied` run in the async handler, they become `async fn` and their callers
`.await` them; the precious `Sink` method becomes async (a dedicated async method, or an `async_trait`
/ future-returning signature — implementation picks the lowest-ripple form).

- **Alternatives considered:**
  - *Sync `try_send` retry/spin loop* — keeps the trait sync but busy-waits or sleeps on a runtime
    thread; wasteful and still crude. Rejected.
  - *`blocking_send`* — has no timeout and panics if called on an async runtime thread (which is where
    emission runs). Rejected.
  - *Unbounded channel* — removes drops but also removes backpressure and bounds memory only by OOM;
    a runaway producer becomes an availability risk. Rejected — the bound is the point.

### D2 — Rent durability from the platform now (stdout + checkpointing collector), not an on-box store

The durable at-least-once outbox is the node-local stdout log file plus a checkpointing collector;
the box's only obligation is to stop dropping on its own hop and hand off cleanly. This is the "Rent"
rung of the build-vs-adopt gate and keeps the box stateless (ephemeral, scale-to-zero, cattle — no
volume, no disk-full failure mode, no backup/rotation on the box).

- **Alternatives considered:**
  - *On-box redb `DurableSink` now* — deferred to D3; unnecessary in the control-plane model and adds
    on-box state.
  - *Synchronous durable handoff before response ack* — adds per-request latency and couples request
    availability to the sink. Rejected for the hot path.

### D3 — redb-backed `DurableSink` is the documented adopt-later impl behind `Sink` (ADR)

> **Decision (build-vs-adopt):** For the standalone / no-collector deployment, *adopt* `redb` (a
> pure-Rust, ACID, single-file embedded store) behind a new `DurableSink` implementing the existing
> `Sink` port — **later**, not in this change.
>
> **Trigger:** a no-collector standalone box must itself be billing-grade (own end-to-end durability
> across restarts without relying on a node-side log tail).
>
> **Why redb over the field:** pure Rust builds cleanly on musl/Docker with no extra C toolchain
> (unlike `rusqlite`/`libsqlite3-sys`, and we already fight `aws-lc-sys`'s C build); actively
> maintained (unlike `sled`, which is beta/unmaintained); and adopting a proven store beats
> hand-writing an append-log + fsync + crash-torn-write recovery, which is exactly the
> reliability-critical work the build-vs-adopt gate says not to build.
>
> **Why deferred:** the control-plane model already has a durable outbox (D2); adding on-box state
> now would be cost without benefit. The `Sink` port + `event_id` mean the swap is an impl selection,
> not an emission-site rewrite.

### D4 — Dropped usage/audit is an SLO signal, not a routine counter

`runlet_events_dropped_total` keeps its name but its *meaning* sharpens: any increment is a genuine
revenue/compliance leak and is intended for alerting (docs/deployment.md frames it as an SLO). The
generous channel size + timeout defaults are chosen so this counter stays at zero under normal load.
`runlet_log_events_dropped_total` (log channel) is unchanged and remains a routine best-effort gauge.

### D5 — Generous defaults so the timeout path is effectively unreachable

The precious-channel bound and the block-with-timeout duration become config (`config.events`), with
defaults sized so normal bursts drain well within the channel and the timeout is only reached under
pathological, alert-worthy saturation. Exact defaults are tuned during implementation; the bound stays
independent of the log channel's.

## Risks / Trade-offs

- **A slow/stuck writer could add up to `timeout` latency per emission** → keep the timeout small
  (single-digit ms range) and the channel generous so the wait is effectively never hit; emission is
  post-execution so it never delays the JS run itself, only the tail of request handling.
- **Sustained saturation still drops** (this is not on-box durability) → surfaced loudly as an SLO
  signal (D4); the honest fix for the no-collector case is D3, explicitly deferred.
- **Durability depends on correct collector deployment** in the control-plane model → documented as a
  deployment requirement in `docs/deployment.md`; a box crash still loses only events not yet written
  to stdout (the small in-channel window), which the block-with-timeout + flush-on-shutdown minimize.
- **Making the precious emit async ripples through `emit_executed`/`emit_denied` and callers** →
  contained to `handler.rs` (and the batch path if it emits); `record_log` and `runlet-core` are
  untouched. Verify no emission site runs on a blocking thread (a `blocking_send` would be needed
  there, or a hop back to async).
- **Shutdown flush must not hang** → the flush drains only what is already buffered (senders dropped
  first, as today), bounded by channel capacity; it is best-effort with a bounded await.

## Migration Plan

1. Land D1 + D4 + D5 in `events.rs` (+ the async emission ripple in `handler.rs`) behind the existing
   `config.events` toggle; defaults keep current behavior observable (drops → 0 under normal load).
2. Update `docs/deployment.md` to state the durable-outbox reliance on a checkpointing stdout
   collector and to frame `runlet_events_dropped_total` as an SLO alert.
3. No data migration; the event envelope/schema (`v`, `event_id`) is unchanged, so any existing
   downstream consumer keeps working (still at-least-once + dedup by `event_id`).
4. Rollback is config/degrade-safe: reverting to `try_send` restores prior behavior with no schema or
   downstream change.

## Open Questions

- **Async-trait shape:** dedicated `async fn record_precious` on `Sink` vs an `async_trait` vs a
  future-returning method — pick the lowest-ripple form that keeps `Debug`/`dyn` object-safety.
  (Implementation detail; does not affect the spec.)
- **Default numbers:** the precious-channel capacity and the timeout duration — settle during
  implementation against a representative burst profile.
- **Batch path:** confirm whether the `/batch` fan-out emits per item through the same `record()` and
  therefore inherits D1 automatically (expected yes; verify during apply).
