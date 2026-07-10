## 1. Precious-channel emission: block-with-timeout (D1)

- [x] 1.1 In `events.rs`, split the `Sink` port so the precious `usage`/`audit` path is async (a dedicated `async` method or future-returning signature that keeps `dyn`/`Debug` object-safety); leave `record_log` a sync fire-and-forget `try_send`.
- [x] 1.2 Implement the precious path on `LogSink` with `Sender::send_timeout(event, timeout).await`: enqueue on capacity, and on `Elapsed`/closed drop-and-count into the usage/audit dropped counter (no `unwrap`/`expect`, no bare arithmetic — use `fetch_add`).
- [x] 1.3 Thread the configurable block-with-timeout `Duration` into `LogSink` / `EventPipeline::spawn` alongside the existing bounds.

## 2. Config + generous defaults (D5)

- [x] 2.1 Add the precious-channel bound and block-with-timeout duration to the `config.events` block in `runlet/src/config.rs` (human-readable duration; keep the log-channel bound independent).
- [x] 2.2 Choose defaults so the timeout path is effectively unreachable under normal burst load (generous bound, single-digit-ms timeout); document the chosen values inline.

## 3. Async emission ripple in the handler (D1)

- [x] 3.1 Make `emit_executed` / `emit_denied` in `handler.rs` `async` and `.await` the precious enqueue at all call sites.
- [x] 3.2 Verify no emission site runs on a `spawn_blocking` thread (a `blocking_send` or hop-back-to-async would be required there); confirm the `/batch` fan-out emits per item through the same precious path and inherits the new behavior.

## 4. Shutdown flush of the precious channel (D1)

- [x] 4.1 Ensure graceful shutdown drops the last `Sink` (closing the precious sender) and awaits the precious writer's drain before process exit, bounded so it cannot hang.

## 5. SLO metric semantics (D4)

- [x] 5.1 Keep `runlet_events_dropped_total` but reframe it as an SLO/alert signal for a revenue/compliance leak (comment + metric help text); confirm `runlet_log_events_dropped_total` stays the routine best-effort gauge, unchanged.

## 6. Tests

- [x] 6.1 Test: a momentarily-full precious channel that drains within the window enqueues the event (not dropped) — drain a slot mid-flight and assert delivery.
- [x] 6.2 Test: sustained saturation past the timeout drops-and-counts exactly, and the request-side await returns within ~timeout (no unbounded block).
- [x] 6.3 Update / keep the D4 isolation test: a saturated `log` channel never advances the usage/audit dropped counter and never drops a usage/audit event.
- [x] 6.4 Test: graceful shutdown flushes buffered precious events to the writer before exit.

## 7. Docs

- [x] 7.1 Update `docs/deployment.md`: the durable at-least-once outbox is the stdout log file + a checkpointing collector (control-plane model); `runlet_events_dropped_total` is an SLO alert; note the deferred redb `DurableSink` (D3) and its trigger.

## 8. Gate

- [x] 8.1 Run `task clippy` (re-run until clean), `cargo test`, and `cargo fmt --all --check` in Docker before considering the change done.
