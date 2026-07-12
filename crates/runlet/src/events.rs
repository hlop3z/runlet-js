//! Per-tenant usage + audit events (Change C).
//!
//! One unified, versioned envelope is emitted per request — a `usage` event for every executed
//! request (billing/quota-tuning record) and an `audit` event for every request (allowed, or
//! denied-with-reason at a gate; the compliance trail). Both ride a **precious** bounded channel,
//! isolated from a **separate** lossy channel carrying diagnostic `log` events (D4), each drained by
//! a writer task that writes one JSON line per event to stdout (a dedicated event stream a collector
//! routes on the envelope).
//!
//! The precious usage/audit path is **bounded block-with-timeout** (billing-grade-event-hop / D1):
//! [`Sink::record`] awaits [`mpsc::Sender::send_timeout`] up to a small, configurable window and
//! only drops-and-counts if the channel is *still* saturated when the timeout expires — a
//! momentarily full channel no longer silently drops a billing/audit event. Any drop is a genuine
//! revenue/compliance leak, counted into `runlet_events_dropped_total` as an **SLO/alert** signal
//! (D4), not a routine occurrence. The diagnostic `log` path ([`Sink::record_log`]) stays
//! sync fire-and-forget `try_send` (best-effort, lossy). The request path is never blocked
//! unboundedly — waiting is capped by the timeout.
//!
//! The [`Sink`] port + the per-event `event_id` (dedup key) are the seam a durable, billing-grade
//! outbox (the deferred redb `DurableSink`, D3) drops into later without changing the emission
//! sites. Identity lives here in `runlet`, never in `runlet-core` (D6); tenant is an event
//! dimension, never a metric label.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

/// Schema version of the event envelope. Bump on any breaking envelope/body change so a durable
/// consumer can branch on it.
const EVENT_SCHEMA_VERSION: u32 = 1;

/// The unified event envelope. `usage` and `audit` events share these fields; the type-specific
/// payload is flattened in from [`EventBody`] (adding a `type` discriminator).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Event {
    /// Envelope schema version ([`EVENT_SCHEMA_VERSION`]). Serialized as `v`.
    #[serde(rename = "v")]
    version: u32,
    /// Unique per-event id — the idempotency/dedup key a durable outbox consumes. Serialized as
    /// `event_id`.
    #[serde(rename = "event_id")]
    id: String,
    /// Event time, Unix epoch milliseconds.
    ts: u128,
    /// Trusted tenant id this event is attributed to. `None` only when a request was rejected
    /// before any tenant was resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    /// Trusted user id (audit attribution).
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// Tenant plan (quota tier).
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    /// Correlation id shared with the request/trace (`meta.trace_id`).
    trace_id: String,
    /// The type-tagged payload (`type` = `usage` | `audit`).
    #[serde(flatten)]
    body: EventBody,
}

/// The type-specific event payload, internally tagged by `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EventBody {
    /// One executed request's usage (billing dimensions).
    Usage(UsageBody),
    /// One request's terminal decision (compliance trail).
    Audit(AuditBody),
    /// One diagnostic log entry (the always-on tenant stream sink). Rides its **own** bounded
    /// channel, isolated from `usage`/`audit` so log volume can never drop a billing/audit event
    /// (D4). Lossy / observability-grade.
    Log(LogBody),
}

/// One diagnostic log entry streamed to the tenant, projected from a core `LogEntry`. The properties
/// are opaque JSON; the level is the lowercase name.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LogBody {
    /// Severity level (`trace`/`debug`/`info`/`warn`/`error`).
    pub(crate) level: String,
    /// The Serilog-style message template.
    pub(crate) template: String,
    /// The merged named properties (opaque JSON).
    pub(crate) properties: Value,
    /// The rendered message.
    pub(crate) message: String,
    /// Call-order sequence number within the execution.
    pub(crate) seq: u64,
    /// Relative microseconds from execution start (full profile only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) offset_us: Option<u128>,
}

/// Per-request usage dimensions — sourced from the response `meta` the box already computes.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageBody {
    /// Terminal outcome (`success` / `script_error` / `capability_error` / `timeout` / …), the
    /// same taxonomy the metrics use.
    pub(crate) outcome: String,
    /// Wall-clock execution time, microseconds.
    pub(crate) exec_time_us: u128,
    /// Total input size (script + context) in bytes.
    pub(crate) input_bytes: usize,
    /// Per-capability operation counts (`db`, `mongo`, `http`, `mail`, `s3`, `redis`, `amq`,
    /// `auth`) — the metered downstream work, including broker-drained egress.
    pub(crate) ops: CapabilityOps,
}

/// Operation counts per capability for one request.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct CapabilityOps {
    /// `db` operations.
    pub(crate) db: usize,
    /// `mongo` operations.
    pub(crate) mongo: usize,
    /// `http` (`api`) requests.
    pub(crate) http: usize,
    /// `mail` operations.
    pub(crate) mail: usize,
    /// `s3` operations.
    pub(crate) s3: usize,
    /// `redis` operations.
    pub(crate) redis: usize,
    /// `amq` operations.
    pub(crate) amq: usize,
    /// `auth` operations.
    pub(crate) auth: usize,
}

/// Per-request decision — `allowed` when the request ran, or `denied` with a machine-readable
/// `reason` code when a gate terminated it (optionally with `detail`, e.g. quota plan/limit/usage).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuditBody {
    /// `allowed` or `denied`.
    pub(crate) decision: &'static str,
    /// The reject reason code (the response error code), when denied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    /// Optional decision detail (e.g. `{plan, limit, usage}` for quota; `{entitlement}` for authz).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<Value>,
}

impl Event {
    /// Builds an event with a fresh `event_id` + timestamp, attributed to the given identity.
    pub(crate) fn new(
        tenant: Option<String>,
        user: Option<String>,
        plan: Option<String>,
        trace_id: String,
        body: EventBody,
    ) -> Self {
        Self {
            version: EVENT_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            ts: now_unix_millis(),
            tenant,
            user,
            plan,
            trace_id,
            body,
        }
    }
}

/// Current Unix time in milliseconds; `0` if the clock is before the epoch (never blocks/panics).
fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |delta| delta.as_millis())
}

/// An event sink. `record`/`record_log` must never fail the request path or block it *unboundedly*.
/// `Debug` is required so `AppState` (which holds an `Arc<dyn Sink>`) can derive it.
pub(crate) trait Sink: Send + Sync + fmt::Debug {
    /// Hands a **precious** `usage`/`audit` event to the sink via bounded block-with-timeout (D1):
    /// the returned future awaits capacity up to the sink's configured window and only
    /// drops-and-counts if the channel is still saturated when the window expires. Returns a boxed
    /// future so the trait stays `dyn`/`Debug` object-safe with **no new dependency** (no
    /// `async_trait`); the caller `.await`s it from the async request handler (emission runs after
    /// the `spawn_blocking` execution returns, never on a blocking thread).
    fn record(&self, event: Event) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    /// Hands a **diagnostic** `log` event to the sink's **separate** channel (D4), so a chatty
    /// script's logs can never starve `usage`/`audit`. Sync fire-and-forget: drops (never awaits)
    /// under backpressure — diagnostics stay best-effort/lossy.
    fn record_log(&self, event: Event);
}

/// The lossy, observability-grade sink: two bounded channels — one for the precious `usage`/`audit`
/// events, a **separate** one for diagnostic `log` events (D4) — each drained by its own writer task
/// emitting one JSON line per event to stdout. A full channel drops the event and increments its
/// dropped counter, so log volume degrades only logs, never billing/audit.
#[derive(Debug)]
struct LogSink {
    /// Bounded sender for the **precious** `usage`/`audit` events.
    tx: mpsc::Sender<Event>,
    /// Count of `usage`/`audit` events dropped (channel still saturated at timeout / closed) — an
    /// SLO/alert signal (D4), a genuine revenue/compliance leak, not a routine occurrence.
    dropped: Arc<AtomicU64>,
    /// Block-with-timeout window for the precious enqueue (D1/D5): how long `record` waits for
    /// capacity before dropping-and-counting. Kept small (single-digit ms) so a stuck writer adds
    /// at most this to a request's tail.
    block_timeout: Duration,
    /// Bounded sender for diagnostic `log` events (the isolated, lossy channel).
    log_tx: mpsc::Sender<Event>,
    /// Count of `log` events dropped (full/closed channel) — a routine best-effort gauge, unchanged.
    log_dropped: Arc<AtomicU64>,
}

impl Sink for LogSink {
    fn record(&self, event: Event) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            // Wait for capacity up to `block_timeout`; drop-and-count only on timeout (still
            // saturated) or a closed channel. `send_timeout` returns the event inside the error,
            // which we discard — the `event_id` dedup key means a downstream consumer never sees it.
            if self
                .tx
                .send_timeout(event, self.block_timeout)
                .await
                .is_err()
            {
                let _ = self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        })
    }

    fn record_log(&self, event: Event) {
        if self.log_tx.try_send(event).is_err() {
            let _ = self.log_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Owns both writer tasks so buffered events can be flushed on shutdown, and exposes the dropped
/// counters for the `/metrics` backpressure gauges.
pub(crate) struct EventPipeline {
    /// The `usage`/`audit` writer task.
    writer: JoinHandle<()>,
    /// The isolated `log` writer task.
    log_writer: JoinHandle<()>,
    /// Shared `usage`/`audit` dropped-events counter.
    dropped: Arc<AtomicU64>,
    /// Shared `log` dropped-events counter.
    log_dropped: Arc<AtomicU64>,
}

impl EventPipeline {
    /// Spawns both writer tasks and returns the pipeline plus the [`Sink`] to place in `AppState`.
    /// `bound` is the **precious** `usage`/`audit` channel capacity, `log_bound` the isolated log
    /// channel's, and `block_timeout` the precious enqueue's block-with-timeout window (D1/D5):
    /// the log channel drops immediately on backpressure, while the precious channel drops only if
    /// still saturated when `block_timeout` expires.
    pub(crate) fn spawn(
        bound: usize,
        log_bound: usize,
        block_timeout: Duration,
    ) -> (Self, Arc<dyn Sink>) {
        let (tx, rx) = mpsc::channel(bound.max(1));
        let (log_tx, log_rx) = mpsc::channel(log_bound.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let log_dropped = Arc::new(AtomicU64::new(0));
        let sink: Arc<dyn Sink> = Arc::new(LogSink {
            tx,
            dropped: Arc::clone(&dropped),
            block_timeout,
            log_tx,
            log_dropped: Arc::clone(&log_dropped),
        });
        let writer = tokio::spawn(writer_loop(rx));
        let log_writer = tokio::spawn(writer_loop(log_rx));
        (
            Self {
                writer,
                log_writer,
                dropped,
                log_dropped,
            },
            sink,
        )
    }

    /// A shared handle to the `usage`/`audit` dropped-events counter, for the `/metrics`
    /// backpressure gauge (`runlet_events_dropped_total`), read live at scrape time.
    pub(crate) fn dropped_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }

    /// A shared handle to the diagnostic-log dropped-events counter, for the
    /// `runlet_log_events_dropped_total` gauge.
    pub(crate) fn log_dropped_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.log_dropped)
    }

    /// Flushes the precious usage/audit channel (D1) then the log channel on shutdown: the caller
    /// drops the last [`Sink`] (closing both channels) before calling this, so each writer drains
    /// its remainder and exits, which this awaits. **Bounded** by [`Self::FLUSH_BUDGET`] so a stuck
    /// writer can never hang process exit — on timeout we log and proceed (at-most a small buffered
    /// window is lost, the same window the collector re-reads from stdout).
    pub(crate) async fn shutdown(self) {
        match timeout(Self::FLUSH_BUDGET, self.writer).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!("event writer task join error: {err}"),
            Err(_elapsed) => {
                tracing::warn!("usage/audit event flush exceeded shutdown budget; proceeding");
            }
        }
        match timeout(Self::FLUSH_BUDGET, self.log_writer).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!("log event writer task join error: {err}"),
            Err(_elapsed) => {
                tracing::warn!("log event flush exceeded shutdown budget; proceeding");
            }
        }
    }

    /// Upper bound on how long shutdown waits for each writer to drain its channel, so graceful
    /// shutdown flushes the precious channel (D1) but can never hang on a stuck stdout writer.
    const FLUSH_BUDGET: Duration = Duration::from_secs(5);
}

/// Drains events and writes one JSON line each to stdout until all senders drop.
#[expect(
    clippy::print_stdout,
    reason = "the event stream IS stdout by design (D5): a distinct JSON line per event the \
              collector tails, decoupled from the tracing/OTLP path so a collector outage cannot \
              lose events"
)]
async fn writer_loop(mut rx: mpsc::Receiver<Event>) {
    while let Some(event) = rx.recv().await {
        match serde_json::to_string(&event) {
            Ok(line) => println!("{line}"),
            Err(err) => tracing::warn!("event serialize error: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Envelope serialization + the non-blocking drop-on-full contract.

    use super::{
        AtomicU64, AuditBody, CapabilityOps, Duration, Event, EventBody, EventPipeline, LogBody,
        LogSink, Ordering, Sink, UsageBody, Value, mpsc,
    };
    use std::sync::Arc;
    use std::time::Instant;

    /// A representative `usage` event.
    fn usage_event() -> Event {
        Event::new(
            Some("ws_acme".to_owned()),
            Some("u_1".to_owned()),
            Some("pro".to_owned()),
            "trace-abc".to_owned(),
            EventBody::Usage(UsageBody {
                outcome: "success".to_owned(),
                exec_time_us: 5,
                input_bytes: 10,
                ops: CapabilityOps::default(),
            }),
        )
    }

    /// A representative `log` event.
    fn log_event() -> Event {
        Event::new(
            Some("ws_acme".to_owned()),
            Some("u_1".to_owned()),
            Some("pro".to_owned()),
            "trace-abc".to_owned(),
            EventBody::Log(LogBody {
                level: "info".to_owned(),
                template: "hi {n}".to_owned(),
                properties: Value::from("{}"),
                message: "hi 1".to_owned(),
                seq: 0,
                offset_us: Some(3),
            }),
        )
    }

    /// The envelope serializes with every shared field, the flattened `type`, and a unique id.
    #[test]
    fn envelope_serializes_with_fields_and_unique_id() {
        let first = usage_event();
        let second = usage_event();
        assert_ne!(
            first.id, second.id,
            "event_id is unique per event (the outbox dedup key)"
        );
        let json = serde_json::to_string(&first).unwrap_or_default();
        for field in [
            "\"v\":1",
            "\"event_id\"",
            "\"trace_id\":\"trace-abc\"",
            "\"type\":\"usage\"",
            "\"tenant\":\"ws_acme\"",
            "\"outcome\":\"success\"",
        ] {
            assert!(json.contains(field), "missing {field} in {json}");
        }
    }

    /// A denied audit event carries the reason + detail and the `audit` type.
    #[test]
    fn audit_event_serializes_reason_and_detail() {
        let event = Event::new(
            Some("ws_acme".to_owned()),
            Some("u_1".to_owned()),
            Some("free".to_owned()),
            "trace-xyz".to_owned(),
            EventBody::Audit(AuditBody {
                decision: "denied",
                reason: Some("QUOTA_EXCEEDED".to_owned()),
                detail: Some(Value::from("detail")),
            }),
        );
        let json = serde_json::to_string(&event).unwrap_or_default();
        assert!(json.contains("\"type\":\"audit\""));
        assert!(json.contains("\"decision\":\"denied\""));
        assert!(json.contains("\"reason\":\"QUOTA_EXCEEDED\""));
    }

    /// `record` never blocks *unboundedly*: once the bounded channel is saturated past the
    /// block-with-timeout window, further events are dropped-and-counted rather than awaited forever.
    #[tokio::test]
    async fn full_channel_drops_and_counts() {
        // Capacity 1, receiver held but never drained → sustained saturation.
        let (tx, _rx) = mpsc::channel(1);
        let (log_tx, _log_rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let log_dropped = Arc::new(AtomicU64::new(0));
        let sink = LogSink {
            tx,
            dropped: Arc::clone(&dropped),
            block_timeout: Duration::from_millis(5),
            log_tx,
            log_dropped: Arc::clone(&log_dropped),
        };
        sink.record(usage_event()).await; // fills the single slot
        sink.record(usage_event()).await; // still full at timeout → dropped
        sink.record(usage_event()).await; // still full at timeout → dropped
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            2,
            "events beyond the bound are dropped after the timeout, not blocked forever"
        );
    }

    /// 6.1 — a *momentarily* full precious channel that drains within the block-with-timeout window
    /// enqueues the event (never dropped). A slot frees mid-flight (concurrently with the blocked
    /// `record`), so the awaited enqueue completes before the timeout.
    #[tokio::test]
    async fn momentarily_full_precious_channel_enqueues_when_slot_frees() {
        let (tx, mut rx) = mpsc::channel(1);
        let (log_tx, _log_rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let log_dropped = Arc::new(AtomicU64::new(0));
        let sink = LogSink {
            tx,
            dropped: Arc::clone(&dropped),
            // Generous window so the mid-flight drain lands well inside it.
            block_timeout: Duration::from_secs(5),
            log_tx,
            log_dropped: Arc::clone(&log_dropped),
        };
        sink.record(usage_event()).await; // fills the single slot
        // Concurrently: the second enqueue blocks on a full channel while a drainer frees a slot
        // after a short delay. `join!` runs both on one task — the blocked send observes the freed
        // capacity and completes.
        let enqueue = async { sink.record(usage_event()).await };
        let drain = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            rx.recv().await
        };
        let (_enqueued, drained) = tokio::join!(enqueue, drain);
        assert!(drained.is_some(), "the first event was in the channel");
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            0,
            "a momentarily full channel that drains in-window drops nothing"
        );
        assert!(
            rx.recv().await.is_some(),
            "the second event was enqueued once capacity freed"
        );
    }

    /// 6.2 — sustained saturation past the timeout drops-and-counts exactly once, and the
    /// request-side await returns promptly (~timeout), never blocking unboundedly.
    #[tokio::test]
    async fn sustained_saturation_drops_once_and_returns_within_timeout() {
        let (tx, _rx) = mpsc::channel(1); // never drained
        let (log_tx, _log_rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let log_dropped = Arc::new(AtomicU64::new(0));
        let sink = LogSink {
            tx,
            dropped: Arc::clone(&dropped),
            block_timeout: Duration::from_millis(30),
            log_tx,
            log_dropped: Arc::clone(&log_dropped),
        };
        sink.record(usage_event()).await; // fills the single slot
        let started = Instant::now();
        sink.record(usage_event()).await; // blocks ~30ms then drops-and-counts
        let waited = started.elapsed();
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            1,
            "sustained saturation drops exactly one event"
        );
        assert!(
            waited < Duration::from_secs(1),
            "the await returned within ~timeout ({waited:?}), not unbounded"
        );
    }

    /// 4.7 — a saturated log channel drops **log** events (advancing the log counter) while
    /// `usage`/`audit` events on their separate channel are still delivered (D4 isolation). The log
    /// channel has capacity 1 and is never drained; the usage channel has room.
    #[tokio::test]
    async fn saturated_log_channel_never_drops_usage() {
        let (tx, mut rx) = mpsc::channel(8);
        let (log_tx, _log_rx) = mpsc::channel(1); // never drained → saturates immediately
        let dropped = Arc::new(AtomicU64::new(0));
        let log_dropped = Arc::new(AtomicU64::new(0));
        let sink = LogSink {
            tx,
            dropped: Arc::clone(&dropped),
            block_timeout: Duration::from_millis(5),
            log_tx,
            log_dropped: Arc::clone(&log_dropped),
        };
        // Saturate the log channel.
        sink.record_log(log_event()); // fills the single slot
        sink.record_log(log_event()); // dropped
        sink.record_log(log_event()); // dropped
        // Usage on the separate channel (room to spare) is unaffected — enqueues without hitting
        // the block-with-timeout window.
        sink.record(usage_event()).await;
        sink.record(usage_event()).await;
        assert_eq!(
            log_dropped.load(Ordering::Relaxed),
            2,
            "log events beyond the log bound are dropped"
        );
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            0,
            "usage/audit events are never dropped by log backpressure"
        );
        assert!(rx.try_recv().is_ok(), "the usage events were delivered");
        assert!(rx.try_recv().is_ok(), "both usage events were delivered");
    }

    /// 6.4 — graceful shutdown flushes buffered precious events: after the last `Sink` is dropped
    /// (closing the precious channel), the writer drains its remaining buffered events and exits,
    /// which `shutdown` awaits — and it returns well within the flush budget (not the timeout path).
    #[tokio::test]
    async fn shutdown_flushes_buffered_precious_events() {
        let (pipeline, sink) = EventPipeline::spawn(64, 64, Duration::from_millis(5));
        // Buffer several precious events on the channel.
        for _ in 0..8_u32 {
            sink.record(usage_event()).await;
        }
        // Drop the last Sink so both channels close; each writer drains its remainder and exits.
        drop(sink);
        let started = Instant::now();
        pipeline.shutdown().await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the writer drained buffered events and exited promptly (not the timeout fallback)"
        );
    }
}
