//! Response assembly: the `{data, error, meta, effects}` envelope types and the builders that project an outcome or a classified error into an axum HTTP response.

use std::collections::BTreeMap;

use axum::Json;
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use tracing::warn;

use runlet_core::engine::{Effect, EngineError, LogEntry};
use runlet_core::errors::{ErrorCategory, ErrorEnvelope, ErrorOwner, ErrorSource};
use runlet_core::host::ExecMetrics;
use runlet_wire::BackendMetrics;

use crate::broker::SessionError;
use crate::local_io::LocalIoMetric;
use crate::status::{Projected, project_envelope};

use super::{RAW_NULL, RespCfg, request_error};

/// Metadata computed by Rust.
#[derive(Debug, Serialize)]
pub(crate) struct Meta {
    /// Correlation ID — also logged server-side with the raw cause, so support can grep
    /// one ID across the mesh. Present on every response (success and error).
    pub(crate) trace_id: String,
    /// Registered-script key, echoed back when the request executed by key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) key: Option<String>,
    /// Partition key, echoed back when one was supplied (Tier 5 observability).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) partition: Option<String>,
    /// Size of the script in bytes.
    pub(crate) script_bytes: usize,
    /// Size of the context payload in bytes.
    pub(crate) context_bytes: usize,
    /// Total input size in bytes (script + context).
    pub(crate) total_input_bytes: usize,
    /// Execution time in microseconds.
    pub(crate) exec_time_us: u128,
    /// Per-capability operation metrics, keyed by capability name — one entry per capability the
    /// request actually used (empty ones omitted). `http`/`s3` come from the engine outcome, the
    /// driver-backed capabilities from the egress adapter; every entry is the same per-op metric
    /// shape (`meta.io.<name>`). The dynamic replacement for the former fixed `<cap>_requests`
    /// fields (**BREAKING**, D8), so custom dev-registered capabilities meter identically.
    pub(crate) io: BTreeMap<String, Value>,
}

impl Meta {
    /// Creates a new `Meta` with the given correlation ID, sizes, and empty metrics.
    pub(crate) const fn new(
        trace_id: String,
        script_bytes: usize,
        context_bytes: usize,
        exec_time_us: u128,
    ) -> Self {
        Self {
            trace_id,
            key: None,
            partition: None,
            script_bytes,
            context_bytes,
            total_input_bytes: script_bytes.saturating_add(context_bytes),
            exec_time_us,
            io: BTreeMap::new(),
        }
    }

    /// Attaches the registered-script key (echoed back on key-mode requests).
    pub(crate) fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    /// Attaches the partition key (echoed back when supplied).
    pub(crate) fn with_partition(mut self, partition: Option<String>) -> Self {
        self.partition = partition;
        self
    }

    /// Attaches the per-capability metrics into the dynamic `meta.io` map: `http`/`s3` from the
    /// engine outcome, the broker-resolved capabilities (keyed by kind) from the egress adapter, and
    /// the **box-direct** local calls (keyed by logical name, D8) from the local egress. Only
    /// capabilities that actually ran get an entry.
    pub(crate) fn with_metrics(mut self, metrics: ExecMetrics, egress: EgressMetrics) -> Self {
        let EgressMetrics { backend, local } = egress;
        insert_io(&mut self.io, "http", metrics.http);
        insert_io(&mut self.io, "s3", metrics.s3);
        insert_io(&mut self.io, "db", backend.db);
        insert_io(&mut self.io, "mongo", backend.mongo);
        insert_io(&mut self.io, "mail", backend.mail);
        insert_io(&mut self.io, "redis", backend.redis);
        insert_io(&mut self.io, "amq", backend.amq);
        insert_io(&mut self.io, "auth", backend.auth);
        for (name, ops) in local {
            insert_io(&mut self.io, &name, ops);
        }
        self
    }
}

/// The drained egress metrics for one execution: the broker's per-kind [`BackendMetrics`] plus the
/// box-direct local per-op metrics keyed by logical name (byo-capabilities D8). Both feed the
/// dynamic `meta.io` map.
#[derive(Debug, Default)]
pub(crate) struct EgressMetrics {
    /// Broker-resolved per-capability metrics (keyed by kind: `db`/`redis`/…).
    pub(crate) backend: BackendMetrics,
    /// Box-direct local per-op metrics, keyed by logical resource name.
    pub(crate) local: BTreeMap<String, Vec<LocalIoMetric>>,
}

/// Serializes a capability's per-op metrics into the `meta.io` map under `name`, skipping the
/// entry entirely when the capability made no calls (so `meta.io` carries only used capabilities).
pub(crate) fn insert_io<T: Serialize>(
    io: &mut BTreeMap<String, Value>,
    name: &str,
    metrics: Vec<T>,
) {
    if metrics.is_empty() {
        return;
    }
    if let Ok(value) = serde_json::to_value(metrics) {
        let _prev = io.insert(name.to_owned(), value);
    }
}

/// Counts the operations recorded for capability `name` in the `meta.io` map (0 if absent).
pub(crate) fn io_count(io: &BTreeMap<String, Value>, name: &str) -> usize {
    io.get(name).and_then(Value::as_array).map_or(0, Vec::len)
}

/// `skip_serializing_if` predicate: an empty effects list is omitted, so a run that never called
/// `emit` stays byte-compatible with the prior `{data, error, meta}` response contract.
pub(crate) const fn effects_empty(effects: &[Effect]) -> bool {
    effects.is_empty()
}

/// Success response: JS-produced `{data, error}` as borrowed `RawValue` + Rust meta, plus the
/// tagged `emit` effects (omitted when empty) and — only when the trusted gateway requested
/// diagnostic capture — the `logs` mirror (omitted otherwise, keeping the response byte-compatible).
#[derive(Debug, Serialize)]
pub(crate) struct Response<'a> {
    /// The data field from the JS handler (borrowed, never copied).
    pub(crate) data: &'a RawValue,
    /// The error field from the JS handler (borrowed, never copied; D1 passthrough).
    pub(crate) error: &'a RawValue,
    /// Metadata computed by Rust.
    pub(crate) meta: Meta,
    /// The ordered `emit(kind, value)` effects; absent when the handler emitted nothing.
    #[serde(skip_serializing_if = "effects_empty")]
    pub(crate) effects: &'a [Effect],
    /// The gateway-gated diagnostic `logs` mirror; `None` (omitted) unless capture was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) logs: Option<&'a [LogEntry]>,
}

/// System-error response: `data` is `null`, `error` is the structured envelope. Carries any
/// effects emitted before the failure (capture-on-failure); the list is omitted when empty. The
/// `logs` mirror is present only when the trusted gateway requested capture (capture-on-failure).
#[derive(Debug, Serialize)]
pub(crate) struct SystemErrorResponse<'a> {
    /// Always `null` on a system error.
    pub(crate) data: Option<()>,
    /// The structured error envelope.
    pub(crate) error: ErrorEnvelope,
    /// Metadata computed by Rust.
    pub(crate) meta: Meta,
    /// Effects emitted before the failure; absent when none.
    #[serde(skip_serializing_if = "effects_empty")]
    pub(crate) effects: &'a [Effect],
    /// The gateway-gated diagnostic `logs` mirror; `None` (omitted) unless capture was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) logs: Option<&'a [LogEntry]>,
}

/// Envelope parsed from the JS response — borrows from the source string.
#[derive(Deserialize)]
pub(crate) struct Envelope<'a> {
    /// Raw data from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    pub(crate) data: &'a RawValue,
    /// Raw error from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    pub(crate) error: &'a RawValue,
}

/// Returns a reference to the pre-allocated `null` raw value.
pub(crate) fn raw_null_ref() -> &'static RawValue {
    &RAW_NULL
}

/// Maps a [`SessionError`] (opening the broker session) to its classified envelope: a
/// resolution failure is a caller fault, an unreachable/absent broker is a retryable operator
/// fault (`EGRESS_UNAVAILABLE`), a protocol slip a non-retryable operator fault (`EGRESS_PROTOCOL`).
/// The HTTP status is a *projection* of the envelope's `(retryable, owner)` (design D6) at the
/// response site — shared by single-`/execute` (which sets the status) and per-item `/batch` (which
/// renders the envelope inside a `200` batch).
pub(crate) fn session_error_envelope(err: SessionError) -> ErrorEnvelope {
    match err {
        SessionError::Resolve { code, message } => request_error(&code, message),
        SessionError::Unavailable(message) => ErrorEnvelope::new(
            ErrorCategory::Runtime,
            ErrorSource::Engine,
            "EGRESS_UNAVAILABLE".to_owned(),
            true,
            ErrorOwner::Operator,
        )
        .with_message(message),
        SessionError::Protocol(_raw) => ErrorEnvelope::new(
            ErrorCategory::Runtime,
            ErrorSource::Engine,
            "EGRESS_PROTOCOL".to_owned(),
            false,
            ErrorOwner::Operator,
        )
        .with_message("egress protocol error".to_owned()),
    }
}

/// Maps a [`SessionError`] to the single-`/execute` HTTP response, projecting the status from the
/// envelope (`EGRESS_UNAVAILABLE ⇒ 503 + Retry-After`, `EGRESS_PROTOCOL ⇒ 409`, resolve ⇒ `400`).
pub(crate) fn session_error_response(err: SessionError, meta: Meta, cfg: RespCfg) -> AxumResponse {
    projected_error_response(session_error_envelope(err), meta, cfg)
}

/// Builds the success response, or a `MALFORMED_RESPONSE` error if the JS envelope
/// can't be parsed.
///
/// Secrets need no output scrubbing: their plaintext never enters JS — it stays
/// Rust-side as opaque handles (see `sys.rs`), so a script can only ever return the
/// `"[secret:NAME]"` placeholder, never the value. The `{data,error}` borrow stays
/// zero-copy.
pub(crate) fn success_response(
    js_json: &str,
    meta: Meta,
    effects: &[Effect],
    logs: Option<&[LogEntry]>,
    cfg: RespCfg,
) -> AxumResponse {
    match serde_json::from_str::<Envelope<'_>>(js_json) {
        Ok(env) => handler_envelope_response(&env, meta, effects, logs, cfg),
        Err(parse_err) => engine_error_response(
            EngineError::Malformed(format!("malformed handler response: {parse_err}")),
            meta,
            effects,
            logs,
            cfg,
        ),
    }
}

/// Serializes the handler's own `{data, error}` output at a status *projected from the returned
/// error* (design D5): `2xx` **iff** `error` is null; otherwise the handler's opt-in top-level
/// boolean `retryable` picks the status (`true ⇒ 503` + `Retry-After`, `false`/absent ⇒ `422`). The
/// body — both `data` and `error` — is passed through **verbatim** (invariant D1); the box only
/// *reads* the one key to set the status line, never rewrites the payload.
pub(crate) fn handler_envelope_response(
    env: &Envelope<'_>,
    meta: Meta,
    effects: &[Effect],
    logs: Option<&[LogEntry]>,
    cfg: RespCfg,
) -> AxumResponse {
    let projected = handler_status(env.error);
    let status = StatusCode::from_u16(projected.status).unwrap_or(StatusCode::OK);
    let mut response = (
        status,
        Json(Response {
            data: env.data,
            error: env.error,
            meta,
            effects,
            logs,
        }),
    )
        .into_response();
    if projected.retry_after {
        add_retry_after(&mut response, cfg.retry_after_seconds);
    }
    response
}

/// A handler-returned envelope carrying no top-level `retryable` key is an ordinary business error.
#[derive(Deserialize)]
pub(crate) struct HandlerRetryable {
    /// The opt-in retry hint the box projects to the status line (`503`/`422`); absent ⇒ park.
    #[serde(default)]
    pub(crate) retryable: Option<bool>,
}

/// Projects a handler-returned `error` value onto the HTTP status line (D5). A JSON-null `error`
/// is a real success (`200`); any non-null `error` is never `2xx` — it parks at `422` unless the
/// handler opted into retry with a top-level `retryable: true` (then `503` + `Retry-After`). A
/// non-object or `retryable`-less error is treated as absent ⇒ `422`.
pub(crate) fn handler_status(error: &RawValue) -> Projected {
    if error.get().trim() == "null" {
        return Projected {
            status: 200,
            retry_after: false,
        };
    }
    let retryable = serde_json::from_str::<HandlerRetryable>(error.get())
        .ok()
        .and_then(|parsed| parsed.retryable);
    if retryable == Some(true) {
        Projected {
            status: 503,
            retry_after: true,
        }
    } else {
        Projected {
            status: 422,
            retry_after: false,
        }
    }
}

/// Maps a classified [`EngineError`] to its projected HTTP response (design D6), and logs the
/// full (raw) error server-side keyed by `trace_id` — so the raw cause is always captured for
/// support even when `error_debug` strips it from the response.
pub(crate) fn engine_error_response(
    err: EngineError,
    meta: Meta,
    effects: &[Effect],
    logs: Option<&[LogEntry]>,
    cfg: RespCfg,
) -> AxumResponse {
    warn!(trace_id = %meta.trace_id, error = ?err, "execute system error");
    let envelope = err.into_envelope(cfg.error_debug, cfg.timeout_retryable);
    projected_error_response_with_effects(envelope, meta, effects, logs, cfg)
}

/// Serializes a classified system error, projecting its HTTP status from the envelope's
/// `(retryable, owner, code)` (the single source of truth, design D6) and attaching a `Retry-After`
/// header on the retryable `5xx` class. The one place a classified fault becomes an HTTP status.
pub(crate) fn projected_error_response(
    error: ErrorEnvelope,
    meta: Meta,
    cfg: RespCfg,
) -> AxumResponse {
    projected_error_response_with_effects(error, meta, &[], None, cfg)
}

/// [`projected_error_response`] carrying any effects captured before an execution error
/// (capture-on-failure) and — when the trusted gateway requested capture — the diagnostic `logs`
/// mirror. Pre-execution errors have no effects/logs and go through the thin wrapper.
pub(crate) fn projected_error_response_with_effects(
    error: ErrorEnvelope,
    meta: Meta,
    effects: &[Effect],
    logs: Option<&[LogEntry]>,
    cfg: RespCfg,
) -> AxumResponse {
    let projected = project_envelope(&error);
    let mut response =
        system_error_response_with_effects(error, projected.status, meta, effects, logs);
    if projected.retry_after {
        add_retry_after(&mut response, cfg.retry_after_seconds);
    }
    response
}

/// Attaches (replacing any existing) the `Retry-After` header as a delay in seconds. Seeded from
/// the configured default — the box's circuit breakers live in the broker, so there is no local
/// cool-down to read; the status already says "retry", the header only bounds the backoff.
pub(crate) fn add_retry_after(response: &mut AxumResponse, seconds: u32) {
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        let _prev = response.headers_mut().insert(RETRY_AFTER, value);
    }
}

/// Serializes a `{ data: null, error, meta }` response at the given status (no effects, no logs).
pub(crate) fn system_error_response(error: ErrorEnvelope, status: u16, meta: Meta) -> AxumResponse {
    system_error_response_with_effects(error, status, meta, &[], None)
}

/// Serializes a `{ data: null, error, meta, effects?, logs? }` response at the given status,
/// carrying any effects captured before the failure (omitted when empty) and the gateway-gated
/// diagnostic `logs` mirror (omitted unless capture was requested).
pub(crate) fn system_error_response_with_effects(
    error: ErrorEnvelope,
    status: u16,
    meta: Meta,
    effects: &[Effect],
    logs: Option<&[LogEntry]>,
) -> AxumResponse {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        code,
        Json(SystemErrorResponse {
            data: None,
            error,
            meta,
            effects,
            logs,
        }),
    )
        .into_response()
}
