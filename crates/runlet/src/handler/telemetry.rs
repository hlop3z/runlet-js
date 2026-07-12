//! Per-request observability: tracing spans, the metrics endpoint, per-tenant usage/audit/log events, the response-log mirror, and capability-latency recording.

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use serde_json::Value;
use tokio::task;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use uuid::Uuid;

use opentelemetry::global;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::{Status, TraceContextExt as _};

use runlet_core::engine::{EngineError, ExecOutcome, LogEntry};
use runlet_core::host::{ExecMetrics, Outcome};
use runlet_core::metrics::{Capability, Metrics};
use runlet_wire::BackendMetrics;

use crate::events::{AuditBody, CapabilityOps, Event, EventBody, LogBody, UsageBody};
use crate::identity::{RunMode, TrustedIdentity};

use super::{
    AppState, EgressMetrics, Meta, RespCfg, engine_error_response, io_count, success_response,
};

/// Adapts an axum [`HeaderMap`] to the `OTel` [`Extractor`] interface so the W3C
/// `traceparent`/`tracestate` propagator can read the incoming trace context.
pub(crate) struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

/// Builds the `/execute` request span. Continues the edge trace when a valid W3C `traceparent`
/// is present (parent-based sampling honors its decision), else starts a fresh root. Identity
/// attributes and the terminal `outcome` are declared empty and filled later via `Span::current()`.
pub(crate) fn build_request_span(headers: &HeaderMap) -> tracing::Span {
    let span = tracing::info_span!(
        "execute",
        otel.kind = "server",
        tenant = Empty,
        user = Empty,
        plan = Empty,
        outcome = Empty,
    );
    let parent = global::get_text_map_propagator(|prop| prop.extract(&HeaderExtractor(headers)));
    span.set_parent(parent);
    span
}

/// The active `OTel` trace id as a string. Valid (propagated or box-rooted) when tracing is
/// enabled; a fresh UUID when it is disabled (no `OTel` layer ⇒ no valid span context).
pub(crate) fn current_trace_id() -> String {
    let context = tracing::Span::current().context();
    let span_context = context.span().span_context().clone();
    if span_context.is_valid() {
        span_context.trace_id().to_string()
    } else {
        Uuid::new_v4().to_string()
    }
}

/// Records the trusted identity (tenant/user/plan) as attributes on the current request span.
/// No-op outside trusted mode (identity `None`). These are span attributes only — never metric
/// labels (design D4: identity must not become a metric dimension).
pub(crate) fn record_identity_attrs(identity: Option<&TrustedIdentity>) {
    let Some(id) = identity else { return };
    let span = tracing::Span::current();
    // `record` returns `&Span` for chaining; discard it (a `Copy` reference, so `let _` not `drop`).
    if let Some(tenant) = id.tenant.as_deref() {
        let _ = span.record("tenant", tenant);
    }
    if let Some(user) = id.user.as_deref() {
        let _ = span.record("user", user);
    }
    if let Some(plan) = id.plan.as_deref() {
        let _ = span.record("plan", plan);
    }
}

/// The span `outcome` label for an engine error, mirroring the metric outcome buckets in
/// `Metrics::record_engine_error` so span and metric agree.
pub(crate) const fn engine_error_outcome(err: &EngineError) -> &'static str {
    match *err {
        EngineError::Syntax(_)
        | EngineError::ScriptNotFound(_)
        | EngineError::ModuleNotFound(_)
        | EngineError::HandlerNotDefined
        | EngineError::Script { .. } => "script_error",
        EngineError::Capability(_) => "capability_error",
        EngineError::Timeout { .. } => "timeout",
        EngineError::MemoryLimit => "memory_limit",
        EngineError::Malformed(_) | EngineError::OutputTooLarge { .. } => "malformed_response",
        EngineError::Internal(_) | EngineError::ShuttingDown => "internal_error",
    }
}

/// Records the terminal `outcome` on the current request span, marking the span as errored for
/// any non-success outcome (so trace backends surface it).
pub(crate) fn record_span_outcome(outcome: &str) {
    let span = tracing::Span::current();
    let _ = span.record("outcome", outcome);
    if outcome != "success" {
        span.set_status(Status::error(outcome.to_owned()));
    }
}

/// `GET /metrics` — Prometheus text exposition of the process-wide counters and live
/// gauges (bulkhead permits read off the semaphore).
pub(crate) async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let available = state.limiter.available_permits();
    // The db circuit breaker moved to the broker (it owns the driver connections now); the box
    // reports zero trips, keeping the `runlet_db_breaker_trips_total` series present when the breaker is off.
    let trips = 0_u64;
    let cache = state.host.bytecode_cache_stats();
    let mut body = state
        .metrics
        .render(available, state.bulkhead_capacity, trips, cache);
    // Usage/audit drop counter — an SLO/alert signal, not a routine backpressure gauge
    // (billing-grade-event-hop / D4): the precious channel uses bounded block-with-timeout, so any
    // increment means a usage/audit event was dropped after waiting for capacity — a genuine
    // revenue/compliance leak. Alert on `increase(runlet_events_dropped_total[…]) > 0`. Lives in
    // `runlet` (the event sink), not the `runlet-core` registry. Absent series ⇒ 0.
    if let Some(counter) = state.event_dropped.as_ref() {
        let dropped = counter.load(Ordering::Relaxed);
        body = format!(
            "{body}# HELP runlet_events_dropped_total SLO signal: usage/audit events dropped after the block-with-timeout window (a revenue/compliance leak; alert if > 0).\n\
             # TYPE runlet_events_dropped_total counter\n\
             runlet_events_dropped_total {dropped}\n"
        );
    }
    // Diagnostic-log channel backpressure gauge (D4): the isolated log channel's own dropped counter,
    // separate from usage/audit so a chatty script's dropped logs are visible without conflating them
    // with billing/audit backpressure. Absent series ⇒ 0.
    if let Some(counter) = state.log_event_dropped.as_ref() {
        let dropped = counter.load(Ordering::Relaxed);
        body = format!(
            "{body}# HELP runlet_log_events_dropped_total Diagnostic log events dropped due to a full buffer.\n\
             # TYPE runlet_log_events_dropped_total counter\n\
             runlet_log_events_dropped_total {dropped}\n"
        );
    }
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}

/// Extracts `(tenant, user, plan)` from an optional trusted identity for event attribution.
pub(crate) fn identity_fields(
    identity: Option<&TrustedIdentity>,
) -> (Option<String>, Option<String>, Option<String>) {
    identity.map_or((None, None, None), |id| {
        (id.tenant.clone(), id.user.clone(), id.plan.clone())
    })
}

/// Emits a `usage` event plus an `allowed` audit event for an executed request (Change C). A no-op
/// when event emission is disabled. Every request that reaches execution produces exactly these two.
pub(crate) async fn emit_executed(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    meta: &Meta,
    outcome: &str,
) {
    let Some(sink) = state.events.as_deref() else {
        return;
    };
    let (tenant, user, plan) = identity_fields(identity);
    let ops = CapabilityOps {
        db: io_count(&meta.io, "db"),
        mongo: io_count(&meta.io, "mongo"),
        http: io_count(&meta.io, "http"),
        mail: io_count(&meta.io, "mail"),
        s3: io_count(&meta.io, "s3"),
        redis: io_count(&meta.io, "redis"),
        amq: io_count(&meta.io, "amq"),
        auth: io_count(&meta.io, "auth"),
    };
    let usage = EventBody::Usage(UsageBody {
        outcome: outcome.to_owned(),
        exec_time_us: meta.exec_time_us,
        input_bytes: meta.total_input_bytes,
        ops,
    });
    sink.record(Event::new(
        tenant.clone(),
        user.clone(),
        plan.clone(),
        meta.trace_id.clone(),
        usage,
    ))
    .await;
    let audit = EventBody::Audit(AuditBody {
        decision: "allowed",
        reason: None,
        detail: None,
    });
    sink.record(Event::new(tenant, user, plan, meta.trace_id.clone(), audit))
        .await;
}

/// Emits a `denied` audit event carrying the reject reason code (and optional detail) at a gate.
/// A no-op when event emission is disabled. Async: the precious enqueue is bounded block-with-timeout
/// (D1), awaited here in the async handler (never on a `spawn_blocking` thread).
pub(crate) async fn emit_denied(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    trace_id: &str,
    reason: &str,
    detail: Option<Value>,
) {
    let Some(sink) = state.events.as_deref() else {
        return;
    };
    let (tenant, user, plan) = identity_fields(identity);
    let audit = EventBody::Audit(AuditBody {
        decision: "denied",
        reason: Some(reason.to_owned()),
        detail,
    });
    sink.record(Event::new(tenant, user, plan, trace_id.to_owned(), audit))
        .await;
}

/// The gateway-asserted diagnostic-log routing policy for a request (§3): whether to mirror the
/// captured logs on the response, and whether the run streams to the live tenant stream (§2). Both
/// derive **only** from trusted signals resolved in the identity layer — never a caller body field.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LogPolicy {
    /// Attach the top-level `logs` list to the response (D5, the playground mirror).
    pub(crate) capture: bool,
    /// The execution mode (OQ1): a `Test`/playground run is response-mirror-only and MUST NOT enter
    /// the live stream / billing / audit.
    pub(crate) mode: RunMode,
}

impl LogPolicy {
    /// Resolves the policy from the request's trusted identity. Outside trusted mode there is no
    /// gateway, so capture is off and the mode is live (a caller can neither force capture nor pick
    /// the mode).
    pub(crate) fn resolve(identity: Option<&TrustedIdentity>) -> Self {
        identity.map_or(
            Self {
                capture: false,
                mode: RunMode::Live,
            },
            |id| Self {
                capture: id.capture,
                mode: id.mode,
            },
        )
    }
}

/// Turns the `spawn_blocking` result into the final HTTP response, attaching metrics to
/// `meta` on success and classifying the error otherwise. Also emits the per-request `usage` +
/// `allowed` audit events (the single executed-request event site, Change C), streams the captured
/// diagnostic logs to the tenant (live mode, §2), and mirrors them on the response when the trusted
/// gateway requested capture (§3, both success and error paths).
pub(crate) async fn build_response(
    result: Result<(Result<Outcome, EngineError>, EgressMetrics), task::JoinError>,
    base_meta: Meta,
    cfg: RespCfg,
    state: &AppState,
    identity: Option<&TrustedIdentity>,
) -> AxumResponse {
    let policy = LogPolicy::resolve(identity);
    let metrics: &Metrics = &state.metrics;
    // Record latency for every execution that ran (shed/rejected requests return earlier).
    metrics.observe_execution(base_meta.exec_time_us);
    match result {
        Ok((Ok(exec), egress)) => {
            record_capability_latencies(metrics, &exec.metrics, &egress.backend);
            // Surface the declarative `emit(kind, value)` effects and — when captured — the diagnostic
            // `logs` on the response, on both the success and error paths (capture-on-failure).
            let Outcome {
                result: exec_result,
                effects,
                logs,
                metrics: exec_metrics,
            } = exec;
            let meta = base_meta.with_metrics(exec_metrics, egress);
            // Stream the captured logs to the tenant (live mode only, §2/OQ1) before shaping the
            // response — a test/playground run never enters the live stream.
            stream_logs(state, identity, &meta, &logs, policy.mode);
            let mirror = policy.capture.then_some(logs.as_slice());
            match exec_result {
                ExecOutcome::Success(js_json) => {
                    emit_executed(state, identity, &meta, "success").await;
                    metrics.record_success();
                    record_span_outcome("success");
                    success_response(&js_json, meta, &effects, mirror, cfg)
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome).await;
                    metrics.record_engine_error(&engine_err);
                    record_span_outcome(outcome);
                    engine_error_response(engine_err, meta, &effects, mirror, cfg)
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome).await;
            metrics.record_engine_error(&engine_err);
            record_span_outcome(outcome);
            // No Outcome ⇒ no captured logs; when capture was requested, present an empty list.
            let mirror = policy.capture.then_some::<&[LogEntry]>(&[]);
            engine_error_response(engine_err, base_meta, &[], mirror, cfg)
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome).await;
            metrics.record_engine_error(&engine_err);
            record_span_outcome(outcome);
            let mirror = policy.capture.then_some::<&[LogEntry]>(&[]);
            engine_error_response(engine_err, base_meta, &[], mirror, cfg)
        }
    }
}

/// Streams each captured diagnostic entry to the tenant's isolated log channel (§2, D4) as a `log`
/// event keyed by tenant + `trace_id`. A no-op when events are disabled, when there is nothing to
/// stream, or for a **test/playground** run (OQ1: test logs are response-mirror-only and never enter
/// the live stream). Non-blocking + drop-on-full via the sink's separate log channel.
pub(crate) fn stream_logs(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    meta: &Meta,
    logs: &[LogEntry],
    mode: RunMode,
) {
    if mode == RunMode::Test || logs.is_empty() {
        return;
    }
    let Some(sink) = state.events.as_deref() else {
        return;
    };
    let (tenant, user, plan) = identity_fields(identity);
    for entry in logs {
        let body = EventBody::Log(LogBody {
            level: entry.level.as_str().to_owned(),
            template: entry.template.clone(),
            properties: serde_json::from_str(entry.properties.get()).unwrap_or(Value::Null),
            message: entry.message.clone(),
            seq: entry.seq,
            offset_us: entry.offset_us,
        });
        sink.record_log(Event::new(
            tenant.clone(),
            user.clone(),
            plan.clone(),
            meta.trace_id.clone(),
            body,
        ));
    }
}

/// Feeds every per-op duration from a finished execution into its capability's latency
/// histogram, so `/metrics` can show which downstream is slow, not just total exec time.
/// `http`/`s3` come from the engine outcome; the driver-backed capabilities from the adapter.
pub(crate) fn record_capability_latencies(
    metrics: &Metrics,
    exec: &ExecMetrics,
    backend: &BackendMetrics,
) {
    for metric in &backend.db {
        metrics.observe_op(Capability::Db, metric.duration_us());
    }
    for metric in &backend.mongo {
        metrics.observe_op(Capability::Mongo, metric.duration_us());
    }
    for metric in &exec.http {
        metrics.observe_op(Capability::Http, metric.duration_us());
    }
    for metric in &backend.mail {
        metrics.observe_op(Capability::Mail, metric.duration_us());
    }
    for metric in &exec.s3 {
        metrics.observe_op(Capability::S3, metric.duration_us());
    }
    for metric in &backend.redis {
        metrics.observe_op(Capability::Redis, metric.duration_us());
    }
    for metric in &backend.amq {
        metrics.observe_op(Capability::Amq, metric.duration_us());
    }
    for metric in &backend.auth {
        metrics.observe_op(Capability::Auth, metric.duration_us());
    }
}
