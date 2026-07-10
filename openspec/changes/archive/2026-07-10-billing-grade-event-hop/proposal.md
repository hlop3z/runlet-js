## Why

Usage and audit events are the billing and compliance record, but today they ride a bounded
channel that **silently drops them on backpressure** (`drop-on-full`, counted but not surfaced) and
loses any in-channel event on a hard crash. For a revenue- and compliance-critical stream that is a
real leak: under exactly the load where billing matters most, the box can under-bill or lose an
audit record, and no one is paged. The `Sink` port, the per-event `event_id`, and the isolated
usage/audit channel (separate from the lossy `log` channel) were all built as the seam for making
this stream durable — this change closes the box-side gap so the box can honestly claim it does not
drop a usage or audit event on its own hop.

We deliberately do **not** add on-box durable storage. In the target deployment — the box behind a
control plane / log-shipping collector — a durable at-least-once outbox already exists: the container
runtime writes the box's stdout to a node-local log file, and a checkpointing collector tails it into
the control plane's billing ingest (deduplicating on `event_id`). The box's only unmet obligation is
to stop dropping usage/audit on the in-process hop between `record()` and stdout, and to hand off
cleanly. This is the "Rent" rung of the build-vs-adopt gate: rent durability from the platform now,
and keep a redb-backed `DurableSink` designed-for behind the `Sink` port for the standalone /
no-collector case later.

## What Changes

- **Usage/audit emission changes from drop-on-full to bounded block-with-timeout.** The precious
  usage/audit channel no longer silently drops under backpressure; the emitter briefly waits for
  capacity and only counts a loss if the channel is *still* saturated when a short timeout expires.
  Loss becomes rare and always observable, not the default response to a slow writer.
- **The usage/audit drop path becomes a loud SLO signal**, not a quiet counter — a dropped usage or
  audit event is an operational incident (revenue/compliance leak), surfaced so it can be alerted on.
- **The usage/audit channel is sized generously** and stays isolated from the `log` channel (the D4
  split is preserved), so log volume can never induce a billing/audit drop.
- **Graceful shutdown flushes the usage/audit channel** before exit (best-effort drain already
  exists; this change makes the precious channel's flush a guaranteed part of shutdown ordering).
- **Log events stay lossy** — `record_log` is unchanged (diagnostics remain best-effort, on their own
  channel).
- **Delivery semantics remain at-least-once with `event_id` idempotent dedup** downstream — no
  exactly-once, no ordering guarantee added.
- **A redb-backed `DurableSink` is recorded in `design.md` as the documented future impl** behind the
  existing `Sink` port, with an explicit adopt-later trigger: *a no-collector standalone box must be
  billing-grade*. No storage dependency is added in this change.

## Capabilities

### New Capabilities

_None._ This hardens an existing capability's delivery contract; no new behavioral surface.

### Modified Capabilities

- `tenant-metering`: the **Non-blocking, fail-open emission** requirement changes for the precious
  usage/audit channel — from "under backpressure events are dropped (not awaited)" to bounded
  block-with-timeout (never silently drop; loss only after a short wait for capacity, and always
  surfaced as an SLO signal), with a guaranteed shutdown flush of that channel. The request path must
  still not be blocked unboundedly. Audit events ride the same precious channel and inherit this
  guarantee; the `log` channel stays lossy.

## Impact

- **Code:** `crates/runlet/src/events.rs` — the `Sink::record` backpressure policy (block-with-timeout
  vs `try_send`), the dropped-events metric semantics, and shutdown flush ordering. Emission call
  sites (`handler.rs`, `batch` path) are untouched — they keep calling `record()`/`record_log()`.
- **Config:** the usage/audit channel bound and a new block-with-timeout duration become tunable in
  `runlet` server config (the existing `config.events` block); defaults chosen so the timeout path is
  effectively unreachable under normal load.
- **Metrics:** `runlet_events_dropped_total` semantics sharpen to "a genuine revenue/compliance leak"
  and gain an SLO-alert framing; `runlet_log_events_dropped_total` is unchanged.
- **`runlet-core`:** untouched (identity and events live in `runlet`, never in core).
- **Dependencies:** none added. redb is documented as adopt-later, not introduced.
- **Deployment:** documents the reliance on a checkpointing stdout collector as the durable outbox
  (the control-plane model) in `docs/deployment.md`.
