//! HTTP handler for the `/execute` endpoint.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task;
use tokio::task::JoinSet;
use tracing::field::Empty;
use tracing::{Instrument as _, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use uuid::Uuid;

use opentelemetry::global;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::{Status, TraceContextExt as _};

use runlet_core::config::EngineConfig;
use runlet_core::engine::{Effect, EngineError, ExecOutcome, LogEntry, LogLevel};
use runlet_core::errors::{ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
use runlet_core::host::{CapabilitySet, ExecMetrics, Invocation, LogicHost, Outcome};
use runlet_core::metrics::{Capability, Metrics};
use runlet_core::partition::PartitionLimiter;
use runlet_core::registry::ScriptRegistry;
use runlet_core::s3::S3Config;
use runlet_core::sandbox;
use runlet_core::sys::SysConfig;
use runlet_wire::wire::WireInit;
// The per-capability metric types (`DbMetric`/`HttpMetric`/…) are no longer named here — the
// dynamic `meta.io` map serializes them generically — but `BackendMetrics` (the sidecar drain),
// the `Egress`/`MeteredEgress` ports, and `ct_eq` are still used directly.
use runlet_wire::{BackendMetrics, Egress, MeteredEgress, ct_eq};

use crate::authz::authorize_capabilities;
use crate::config::{BatchConfig, TrustedHeaders};
use crate::events::{AuditBody, CapabilityOps, Event, EventBody, LogBody, Sink, UsageBody};
use crate::identity::{RunMode, TrustedIdentity};
use crate::local_io::{BoxEgress, LocalIoMetric};
use crate::quota::{QuotaExceeded, QuotaGuard, TenantQuota};
use crate::sidecar::{SessionConn, SessionError, SidecarEgress, SidecarTransport, connect_session};
use crate::status::{Projected, project_envelope};

/// Shared application state for the router: the logic host, the script registry, and
/// the concurrency bulkhead.
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    /// The callable logic host (pool + resilience + engine limits) — runs each request.
    pub(crate) host: LogicHost,
    /// Read-only registry of scripts loaded at startup (execute-by-key). Resolved here for
    /// input-sizing/meta before the source is handed to the host as inline code.
    pub(crate) registry: Arc<ScriptRegistry>,
    /// Engine sandbox limits, used outside the host for input-size validation.
    pub(crate) engine_cfg: EngineConfig,
    /// Include `error.debug` (stack traces + raw causes) in responses.
    pub(crate) error_debug: bool,
    /// Bulkhead bounding concurrent executions: a permit is held across the blocking
    /// execution span, and acquisition fast-fails (`429 OVERLOADED`) when saturated so a
    /// slow downstream can't exhaust blocking threads / DB connections.
    pub(crate) limiter: Arc<Semaphore>,
    /// Per-partition fairness (Tier 5): caps concurrency per `X-Partition-Key`. `None` when
    /// disabled. Acquired *before* the global bulkhead so a noisy partition fast-fails on its
    /// own share (`429 PARTITION_OVERLOADED`) while global capacity stays free for others.
    pub(crate) partition_limiter: Option<PartitionLimiter>,
    /// How the box reaches the `fabricd` egress sidecar (local UDS, remote QUIC, or none). The box
    /// links no driver and holds no credentials: a request that names a driver resource in
    /// `config.io` opens a session over this transport to `fabricd`, which resolves the name against
    /// its own operator config and performs the I/O. [`SidecarTransport::None`] ⇒ driver
    /// capabilities are unavailable (`503 EGRESS_UNAVAILABLE`).
    pub(crate) transport: SidecarTransport,
    /// Box-direct local egress bindings (byo-capabilities D8): logical resource name → co-located
    /// loopback endpoint URL. A name in `config.io` bound here resolves **box-direct** (a POST of the
    /// `{action, payload}` envelope) instead of forwarding to the broker; empty = every name forwards
    /// to the broker. Operator-declared in global config, loopback-only (boot-guard validated).
    pub(crate) local_resources: Arc<HashMap<String, String>>,
    /// Shared `reqwest` client for box-direct POSTs (reuses the process rustls/aws-lc-rs stack).
    pub(crate) local_client: reqwest::Client,
    /// Process-wide observability counters, exposed at `GET /metrics`.
    pub(crate) metrics: Arc<Metrics>,
    /// Configured global bulkhead capacity, surfaced as the `_total` permit gauge.
    pub(crate) bulkhead_capacity: usize,
    /// Shared-secret bearer token gating `/execute`. In trusted-header mode this is the
    /// edge→box **service credential** (defense in depth with the `NetworkPolicy`). `None` = no
    /// in-process auth (loopback or `allow_unauthenticated`).
    pub(crate) access_token: Option<Arc<str>>,
    /// Trusted-identity ("nexus edge") mode. When set, `/execute` derives identity from the
    /// configured trusted headers, rejects anonymous/suspended, and keys fairness/cache/egress/
    /// quota off the trusted tenant id. `None` = single-tenant behavior (caller-asserted partition).
    pub(crate) trusted: Option<Arc<TrustedRuntime>>,
    /// Per-tenant usage + audit event sink (Change C). `None` when event emission is disabled;
    /// every emit site is a no-op then. Non-blocking + fail-open (see `events.rs`).
    pub(crate) events: Option<Arc<dyn Sink>>,
    /// Live handle to the dropped-events counter, rendered as `runlet_events_dropped_total` (the
    /// backpressure signal). `None` when events are disabled.
    pub(crate) event_dropped: Option<Arc<AtomicU64>>,
    /// Live handle to the diagnostic-log dropped-events counter, rendered as
    /// `runlet_log_events_dropped_total` (the isolated log-channel backpressure signal, D4). `None`
    /// when events are disabled.
    pub(crate) log_event_dropped: Option<Arc<AtomicU64>>,
    /// `POST /batch` fan-out caps (item count + combined-input / total-response byte bounds). Copied
    /// from server config; the single-`/execute` path never reads it.
    pub(crate) batch: BatchConfig,
    /// Whether a wall-clock `TIMEOUT` is classified retryable (`true ⇒ 503`, `false ⇒ 422`). Only
    /// `TIMEOUT` reads it; `MEMORY_LIMIT`/op-cap stay non-retryable. Fed into `into_envelope`.
    pub(crate) timeout_retryable: bool,
    /// Seconds advertised in the `Retry-After` header on a retryable `503`/`500`.
    pub(crate) retry_after_seconds: u32,
    /// Operator-global default currency for `$` / `money` construction (the last cascade level).
    /// `None` = no global default; a request without `config.currency` then constructs money
    /// currency-less and a `$("19.99")` throws asking for a currency.
    pub(crate) default_currency: Option<Arc<str>>,
}

impl AppState {
    /// The response-shaping policy for this request: debug gating, the `TIMEOUT` retryability knob,
    /// and the `Retry-After` default — threaded together so a response builder takes one `Copy`
    /// value instead of three loose flags.
    const fn resp_cfg(&self) -> RespCfg {
        RespCfg {
            error_debug: self.error_debug,
            timeout_retryable: self.timeout_retryable,
            retry_after_seconds: self.retry_after_seconds,
        }
    }
}

/// Response-shaping policy carried into the error/success builders (a `Copy` bundle so the
/// signatures stay within the argument-count lint).
#[derive(Debug, Clone, Copy)]
struct RespCfg {
    /// Include `error.debug` (stack + raw cause) in the envelope.
    error_debug: bool,
    /// `TIMEOUT` retryability knob (governs only the `TIMEOUT` fault's `retryable`).
    timeout_retryable: bool,
    /// `Retry-After` seconds attached to a retryable `503`/`500`.
    retry_after_seconds: u32,
}

/// Resolved trusted-header-mode runtime state, shared into the handler. Present only when
/// `trusted.enabled`.
#[derive(Debug)]
pub(crate) struct TrustedRuntime {
    /// The configured trusted header names.
    pub(crate) headers: TrustedHeaders,
    /// Coarse capability→required-entitlement gate (empty = no member gating).
    pub(crate) capability_entitlements: HashMap<String, String>,
    /// Per-tenant plan-gated quota accountant. `None` when quota is disabled.
    pub(crate) quota: Option<TenantQuota>,
}

/// Pre-allocated `Box<RawValue>` for `{}` — used as default context.
static DEFAULT_CONTEXT: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("{}".into()).unwrap_or_else(|_err| unreachable!()));

/// Pre-allocated `Box<RawValue>` for `null` — used as default envelope field.
static RAW_NULL: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("null".into()).unwrap_or_else(|_err| unreachable!()));

/// Request body for script execution.
#[derive(Debug, Deserialize)]
pub(crate) struct ExecRequest {
    /// Inline JavaScript source to evaluate (exactly one of `script` / `key`).
    script: Option<String>,
    /// Registered-script key to execute (exactly one of `script` / `key`).
    key: Option<String>,
    /// Caller-asserted partition key for per-partition fairness (Tier 5), single-tenant mode only.
    /// The `X-Partition-Key` header takes precedence over this field. **Ignored in trusted-header
    /// mode**, where the fairness key is the trusted tenant id (a caller cannot pick its bucket).
    #[serde(default)]
    partition: Option<String>,
    /// Raw context passed straight to `QuickJS` — never deserialized in Rust.
    #[serde(default = "default_context")]
    context: Box<RawValue>,
    /// Per-request configuration.
    #[serde(default)]
    config: RequestConfig,
}

/// Resolved script source — inline from the request body or shared from the registry.
#[derive(Debug)]
enum ScriptSource {
    /// Inline `script` field.
    Inline(String),
    /// Registered script resolved from `key`.
    Registered(Arc<str>),
}

impl ScriptSource {
    /// The script text.
    fn as_str(&self) -> &str {
        match self {
            Self::Inline(source) => source.as_str(),
            Self::Registered(source) => source.as_ref(),
        }
    }
}

/// Per-request configuration sent by the caller.
///
/// Driver-backed capabilities carry no connection config here: the request names logical resources
/// in [`io`](Self::io) and the box forwards those names to `fabricd`, which resolves the
/// endpoint/credentials operator-side. `http` (`allowed_hosts`) and `s3` stay
/// script-controlled/in-engine and keep their config.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestConfig {
    /// Allowed hosts for the `http` HTTP client.
    #[serde(default)]
    pub(crate) allowed_hosts: Vec<String>,
    /// S3 presigning config (omit to disable `s3` in JS). Stays in-engine (pure `SigV4` presign).
    #[serde(default)]
    pub(crate) s3: Option<S3Config>,
    /// `$sys` env/secrets context (omit to leave `$sys.env`/`$sys.secrets` empty).
    #[serde(default)]
    pub(crate) sys: Option<SysConfig>,
    /// Logical resources this invocation may reach — a plain allowlist of names (e.g.
    /// `["orders","cache"]`). The box is kind-blind: it forwards the names to `fabricd` (which
    /// resolves each to a kind/endpoint/creds) or, for a name the operator bound box-direct, POSTs
    /// to the co-located endpoint. The request never carries endpoints or credentials.
    #[serde(default)]
    pub(crate) io: RequestIo,
    /// Default currency for `$` / `money` construction in this request (the middle level of the
    /// cascade: explicit arg → this → operator `default_currency`). An ISO 4217 code (e.g.
    /// `"EUR"`); lets a script embed its currency once instead of repeating it per call.
    #[serde(default)]
    pub(crate) currency: Option<String>,
}

/// The `config.io` allowlist: a **flat list of logical resource names** the script may address
/// (byo-capabilities D3). No per-kind structure — "kind" is resolved operator-side (by the broker
/// or the box-direct binding). The allowlist is both the enabled set (`io.call(name, …)` is gated
/// by it) and the set of names forwarded to the broker in `WireInit`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub(crate) struct RequestIo(pub(crate) Vec<String>);

impl RequestIo {
    /// `true` if the request names any egress resource (so the `io` global + an egress port are
    /// wired for this request).
    const fn any(&self) -> bool {
        !self.0.is_empty()
    }

    /// The allowlisted names. Passed to the engine as `CapabilitySet.io`: `io` is injected globally
    /// under `Profile::Full`, and `io.call(name, …)` is gated by this list (an unlisted name is
    /// rejected `RESOURCE_NOT_FOUND` before any egress).
    fn enabled_names(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }

    /// The names that must be resolved by the **broker** — every allowlisted name not bound
    /// box-direct in the global `local_resources` map. Box-direct names are served locally, so they
    /// are never sent to `fabricd` (which would fail to resolve them).
    fn broker_names(&self, local: &HashMap<String, String>) -> Vec<String> {
        self.0
            .iter()
            .filter(|name| !local.contains_key(*name))
            .cloned()
            .collect()
    }
}

/// The `fabricd` session-open message: the flat list of broker-resolved resource names, the
/// per-execution deadline, and the request's trusted tenant id (so `fabricd` scopes resolution to
/// that tenant's bindings). `fabricd` resolves each name against its operator config.
fn wire_init(resources: Vec<String>, timeout: Duration, tenant: Option<&str>) -> WireInit {
    WireInit {
        resources,
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        // The trusted tenant id, sourced only from the trusted-header extractor (never the script).
        // `None` on the single-tenant/loopback path. Its **presence** is itself the multitenant
        // signal: `fabricd` treats any tenant-scoped session as least-privilege-mandatory (the
        // `allow_privileged` opt-out is void), so the box carries no separate privilege flag. See
        // `docs/design/resource-egress.md` (least-privilege / trust model).
        tenant: tenant.map(str::to_owned),
        // The token (QUIC path) is attached by `connect_session` from the transport's auth
        // provider — the box-request layer never sees it.
        token: None,
    }
}

/// Returns a clone of the pre-allocated default context.
fn default_context() -> Box<RawValue> {
    DEFAULT_CONTEXT.clone()
}

/// Metadata computed by Rust.
#[derive(Debug, Serialize)]
struct Meta {
    /// Correlation ID — also logged server-side with the raw cause, so support can grep
    /// one ID across the mesh. Present on every response (success and error).
    trace_id: String,
    /// Registered-script key, echoed back when the request executed by key.
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    /// Partition key, echoed back when one was supplied (Tier 5 observability).
    #[serde(skip_serializing_if = "Option::is_none")]
    partition: Option<String>,
    /// Size of the script in bytes.
    script_bytes: usize,
    /// Size of the context payload in bytes.
    context_bytes: usize,
    /// Total input size in bytes (script + context).
    total_input_bytes: usize,
    /// Execution time in microseconds.
    exec_time_us: u128,
    /// Per-capability operation metrics, keyed by capability name — one entry per capability the
    /// request actually used (empty ones omitted). `http`/`s3` come from the engine outcome, the
    /// driver-backed capabilities from the egress adapter; every entry is the same per-op metric
    /// shape (`meta.io.<name>`). The dynamic replacement for the former fixed `<cap>_requests`
    /// fields (**BREAKING**, D8), so custom dev-registered capabilities meter identically.
    io: BTreeMap<String, Value>,
}

impl Meta {
    /// Creates a new `Meta` with the given correlation ID, sizes, and empty metrics.
    const fn new(
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
    fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    /// Attaches the partition key (echoed back when supplied).
    fn with_partition(mut self, partition: Option<String>) -> Self {
        self.partition = partition;
        self
    }

    /// Attaches the per-capability metrics into the dynamic `meta.io` map: `http`/`s3` from the
    /// engine outcome, the broker-resolved capabilities (keyed by kind) from the egress adapter, and
    /// the **box-direct** local calls (keyed by logical name, D8) from the local egress. Only
    /// capabilities that actually ran get an entry.
    fn with_metrics(mut self, metrics: ExecMetrics, egress: EgressMetrics) -> Self {
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
struct EgressMetrics {
    /// Broker-resolved per-capability metrics (keyed by kind: `db`/`redis`/…).
    backend: BackendMetrics,
    /// Box-direct local per-op metrics, keyed by logical resource name.
    local: BTreeMap<String, Vec<LocalIoMetric>>,
}

/// Serializes a capability's per-op metrics into the `meta.io` map under `name`, skipping the
/// entry entirely when the capability made no calls (so `meta.io` carries only used capabilities).
fn insert_io<T: Serialize>(io: &mut BTreeMap<String, Value>, name: &str, metrics: Vec<T>) {
    if metrics.is_empty() {
        return;
    }
    if let Ok(value) = serde_json::to_value(metrics) {
        let _prev = io.insert(name.to_owned(), value);
    }
}

/// Counts the operations recorded for capability `name` in the `meta.io` map (0 if absent).
fn io_count(io: &BTreeMap<String, Value>, name: &str) -> usize {
    io.get(name).and_then(Value::as_array).map_or(0, Vec::len)
}

/// `skip_serializing_if` predicate: an empty effects list is omitted, so a run that never called
/// `emit` stays byte-compatible with the prior `{data, error, meta}` response contract.
const fn effects_empty(effects: &[Effect]) -> bool {
    effects.is_empty()
}

/// Success response: JS-produced `{data, error}` as borrowed `RawValue` + Rust meta, plus the
/// tagged `emit` effects (omitted when empty) and — only when the trusted gateway requested
/// diagnostic capture — the `logs` mirror (omitted otherwise, keeping the response byte-compatible).
#[derive(Debug, Serialize)]
struct Response<'a> {
    /// The data field from the JS handler (borrowed, never copied).
    data: &'a RawValue,
    /// The error field from the JS handler (borrowed, never copied; D1 passthrough).
    error: &'a RawValue,
    /// Metadata computed by Rust.
    meta: Meta,
    /// The ordered `emit(kind, value)` effects; absent when the handler emitted nothing.
    #[serde(skip_serializing_if = "effects_empty")]
    effects: &'a [Effect],
    /// The gateway-gated diagnostic `logs` mirror; `None` (omitted) unless capture was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    logs: Option<&'a [LogEntry]>,
}

/// System-error response: `data` is `null`, `error` is the structured envelope. Carries any
/// effects emitted before the failure (capture-on-failure); the list is omitted when empty. The
/// `logs` mirror is present only when the trusted gateway requested capture (capture-on-failure).
#[derive(Debug, Serialize)]
struct SystemErrorResponse<'a> {
    /// Always `null` on a system error.
    data: Option<()>,
    /// The structured error envelope.
    error: ErrorEnvelope,
    /// Metadata computed by Rust.
    meta: Meta,
    /// Effects emitted before the failure; absent when none.
    #[serde(skip_serializing_if = "effects_empty")]
    effects: &'a [Effect],
    /// The gateway-gated diagnostic `logs` mirror; `None` (omitted) unless capture was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    logs: Option<&'a [LogEntry]>,
}

/// Envelope parsed from the JS response — borrows from the source string.
#[derive(Deserialize)]
struct Envelope<'a> {
    /// Raw data from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    data: &'a RawValue,
    /// Raw error from JS (zero-copy borrow).
    #[serde(default = "raw_null_ref", borrow)]
    error: &'a RawValue,
}

/// Returns a reference to the pre-allocated `null` raw value.
fn raw_null_ref() -> &'static RawValue {
    &RAW_NULL
}

/// Maps a [`SessionError`] (opening the `fabricd` session) to its classified envelope: a
/// resolution failure is a caller fault, an unreachable/absent sidecar is a retryable operator
/// fault (`EGRESS_UNAVAILABLE`), a protocol slip a non-retryable operator fault (`EGRESS_PROTOCOL`).
/// The HTTP status is a *projection* of the envelope's `(retryable, owner)` (design D6) at the
/// response site — shared by single-`/execute` (which sets the status) and per-item `/batch` (which
/// renders the envelope inside a `200` batch).
fn session_error_envelope(err: SessionError) -> ErrorEnvelope {
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
fn session_error_response(err: SessionError, meta: Meta, cfg: RespCfg) -> AxumResponse {
    projected_error_response(session_error_envelope(err), meta, cfg)
}

/// Adapts an axum [`HeaderMap`] to the `OTel` [`Extractor`] interface so the W3C
/// `traceparent`/`tracestate` propagator can read the incoming trace context.
struct HeaderExtractor<'a>(&'a HeaderMap);

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
fn build_request_span(headers: &HeaderMap) -> tracing::Span {
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
fn current_trace_id() -> String {
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
fn record_identity_attrs(identity: Option<&TrustedIdentity>) {
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
const fn engine_error_outcome(err: &EngineError) -> &'static str {
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
fn record_span_outcome(outcome: &str) {
    let span = tracing::Span::current();
    let _ = span.record("outcome", outcome);
    if outcome != "success" {
        span.set_status(Status::error(outcome.to_owned()));
    }
}

/// Executes a JS `handler(context)` and returns `{data, error, meta}` JSON.
///
/// Takes `Result<Json<…>, JsonRejection>` rather than `Json<…>` so a malformed or
/// type-confused body is handled here as a structured `{data, error, meta}` envelope,
/// instead of axum short-circuiting with its default plain-text rejection.
pub(crate) async fn execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<ExecRequest>, JsonRejection>,
) -> AxumResponse {
    // Wrap the whole request in a span that continues the edge trace (or starts a fresh root);
    // identity attributes and the terminal outcome are recorded on it from within via
    // `Span::current()`, so no threading is needed through the pipeline.
    let span = build_request_span(&headers);
    run_execute(state, headers, payload).instrument(span).await
}

/// The `/execute` request pipeline. Runs inside the request span built by [`execute`], so
/// `tracing::Span::current()` here (and in [`build_response`]) is that span.
#[expect(
    clippy::too_many_lines,
    reason = "linear request pipeline: auth → open egress session → admit → execute → respond"
)]
async fn run_execute(
    state: AppState,
    headers: HeaderMap,
    payload: Result<Json<ExecRequest>, JsonRejection>,
) -> AxumResponse {
    // Edge service credential (defense in depth) — reject before any body work.
    if let Some(rejected) = enforce_auth(&state, &headers) {
        return rejected;
    }

    // The active OTel trace id: propagated from the edge when a `traceparent` was present, the
    // box-started root id when tracing is enabled, or a fresh UUID when tracing is off.
    let trace_id = current_trace_id();

    // Trusted-identity ingress (trusted-header mode only): derive identity solely from the
    // configured trusted headers and reject anonymous / suspended / tenant-less callers *before*
    // any body work — no execution or egress session begins for them.
    let identity = match resolve_identity(&state, &headers, &trace_id) {
        Ok(identity) => identity,
        Err(rejected) => {
            state.metrics.record_rejection();
            return *rejected;
        }
    };
    // Attribute this request to the trusted principal on the span (trusted mode only) — as span
    // attributes, never metric labels (the cardinality invariant, design D4).
    record_identity_attrs(identity.as_ref());
    // The trusted tenant id — sourced only from the extractor (never the script). `None` in
    // single-tenant/loopback mode. Guaranteed `Some` in trusted mode (tenant-less was rejected).
    let tenant = identity.as_ref().and_then(|id| id.tenant.clone());

    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => {
            state.metrics.record_rejection();
            emit_denied(
                &state,
                identity.as_ref(),
                &trace_id,
                "MALFORMED_REQUEST",
                None,
            );
            return malformed_request_response(&state, &rejection);
        }
    };
    let ExecRequest {
        script,
        key,
        partition: body_partition,
        context,
        config,
    } = req;
    // Fairness + cache key (Tier 5). In trusted mode this is the trusted tenant id and any
    // caller-asserted `X-Partition-Key` / `partition` body field is ignored; otherwise the
    // caller-asserted source (single-tenant behavior).
    let caller_asserted = header_partition(&headers).or(body_partition);
    let partition = resolve_partition(identity.as_ref(), caller_asserted);
    let context_bytes = context.get().len();

    // Coarse member-capability authz (trusted mode): reject a member lacking the entitlement a
    // requested capability requires, before any session or execution.
    if let Some(rejected) = enforce_member_authz(&state, identity.as_ref(), &config, &trace_id) {
        state.metrics.record_rejection();
        return *rejected;
    }

    let engine_cfg = state.engine_cfg;
    let cfg = state.resp_cfg();

    // Resolve exactly one of `script` / `key` into the source to execute.
    let source = match resolve_script(script, key.as_deref(), &state.registry) {
        Ok(source) => source,
        Err(rejection) => {
            state.metrics.record_rejection();
            emit_denied(
                &state,
                identity.as_ref(),
                &trace_id,
                "SCRIPT_NOT_FOUND",
                None,
            );
            let (_status, envelope) = *rejection;
            let meta = Meta::new(trace_id, 0, context_bytes, 0)
                .with_key(key)
                .with_partition(partition);
            // The status is a projection of the envelope (`SCRIPT_NOT_FOUND ⇒ 404`,
            // `SCRIPT_XOR_KEY ⇒ 400`); the tuple's status literal is superseded (D6).
            return projected_error_response(envelope, meta, cfg);
        }
    };
    let script_bytes = source.as_str().len();

    // Early validation — reject oversized inputs before spawning a task.
    if let Err((code, message)) = sandbox::validate_input_sizes(
        script_bytes,
        context_bytes,
        engine_cfg.max_script_size,
        engine_cfg.max_context_size,
    ) {
        state.metrics.record_rejection();
        emit_denied(
            &state,
            identity.as_ref(),
            &trace_id,
            "INPUT_TOO_LARGE",
            None,
        );
        let meta = Meta::new(trace_id, script_bytes, context_bytes, 0)
            .with_key(key)
            .with_partition(partition);
        // Oversize input is a caller fault that parks at `413` (projected from the
        // `SCRIPT_TOO_LARGE`/`CONTEXT_TOO_LARGE` code), not a generic `400`.
        return projected_error_response(request_error(code, message), meta, cfg);
    }

    let context_json: String = context.get().into();

    // Per-tenant quota (trusted mode): attribute this execution to the trusted tenant and enforce
    // the plan's hard cap. The guard is held across the execution span (released on drop) and
    // returns the structured over-limit result when at/above the limit.
    let _quota_guard = match enforce_quota(&state, identity.as_ref(), &trace_id, || {
        base_error_meta(
            &trace_id,
            script_bytes,
            context_bytes,
            key.as_deref(),
            partition.as_deref(),
        )
    }) {
        Ok(guard) => guard,
        Err(rejected) => {
            state.metrics.record_rejection();
            return *rejected;
        }
    };

    // Open the `fabricd` egress session only for names the broker must resolve — every allowlisted
    // name **not** bound box-direct in the global `local_resources` map (D8). A box-direct-only
    // request opens no broker session. The box holds no credentials: it sends the broker names + the
    // trusted tenant id; `fabricd` resolves them within that tenant's binding set. An unknown/
    // out-of-tenant name (400), or an unreachable/absent sidecar (503), is rejected here — before
    // admission.
    let broker_names = config.io.broker_names(&state.local_resources);
    let session = if broker_names.is_empty() {
        None
    } else {
        let init = wire_init(broker_names, engine_cfg.timeout(), tenant.as_deref());
        match connect_session(&state.transport, &init).await {
            Ok(conn) => Some(conn),
            Err(err) => {
                state.metrics.record_rejection();
                emit_denied(
                    &state,
                    identity.as_ref(),
                    &trace_id,
                    "EGRESS_UNAVAILABLE",
                    None,
                );
                let meta = Meta::new(trace_id, script_bytes, context_bytes, 0)
                    .with_key(key)
                    .with_partition(partition);
                return session_error_response(err, meta, cfg);
            }
        }
    };

    let start = Instant::now();

    // Acquire the per-partition (Tier 5) then global bulkhead (Tier 1) permits.
    let busy_meta = base_error_meta(
        &trace_id,
        script_bytes,
        context_bytes,
        key.as_deref(),
        partition.as_deref(),
    );
    let (partition_permit, permit) = match admit(&state, partition.as_deref(), busy_meta) {
        Ok(permits) => permits,
        Err(shed) => {
            emit_denied(&state, identity.as_ref(), &trace_id, "OVERLOADED", None);
            return *shed;
        }
    };

    // Namespace the bytecode cache by the fairness key — in trusted mode the trusted tenant id —
    // so byte-identical source from different tenants never shares an entry (no cross-tenant dedup
    // / compile-timing leak). Cloned because the shared core takes it by value and `partition` is
    // still needed for `meta` after the await.
    let cache_ns = partition.clone();
    // The trusted per-request log-floor override (D6/OQ2), only meaningful in trusted mode.
    let log_floor = identity.as_ref().and_then(|id| id.log_level);
    let result = execute_blocking(ExecuteBlocking {
        host: state.host.clone(),
        handle: Handle::current(),
        timeout: engine_cfg.timeout(),
        session,
        local_resources: Arc::clone(&state.local_resources),
        local_client: state.local_client.clone(),
        source,
        context_json,
        config,
        cache_ns,
        log_floor,
        default_currency: state.default_currency.clone(),
    })
    .await;

    // Execution finished — free the bulkhead + per-partition permits for the next request.
    drop(permit);
    drop(partition_permit);

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = Meta::new(trace_id, script_bytes, context_bytes, exec_time_us)
        .with_key(key)
        .with_partition(partition);
    build_response(result, base_meta, cfg, &state, identity.as_ref())
}

/// Inputs for the shared blocking-execution core (grouped so the call sites and the
/// `spawn_blocking` closure stay within the argument-count lint).
struct ExecuteBlocking {
    /// The callable logic host (cloned per invocation; `Arc`-backed).
    host: LogicHost,
    /// Runtime handle to drive the sidecar socket I/O via `block_on` on the blocking thread.
    handle: Handle,
    /// Per-execution wall-clock budget bounding every egress round-trip.
    timeout: Duration,
    /// The pre-connected `fabricd` session, when the request named broker-resolved resources.
    session: Option<SessionConn>,
    /// Box-direct local egress bindings (name → loopback URL), consulted before the broker (D8).
    local_resources: Arc<HashMap<String, String>>,
    /// Shared `reqwest` client for the box-direct POSTs.
    local_client: reqwest::Client,
    /// The resolved script source (inline or registered).
    source: ScriptSource,
    /// Raw context JSON handed straight to `QuickJS`.
    context_json: String,
    /// Per-request capability configuration.
    config: RequestConfig,
    /// Bytecode-cache namespace (the fairness key) — keeps byte-identical source from different
    /// tenants from sharing a cache entry.
    cache_ns: Option<String>,
    /// Trusted per-request diagnostic-log floor override (D6/OQ2). `None` uses the host's configured
    /// floor; the gateway lowers it for a capture run.
    log_floor: Option<LogLevel>,
    /// Operator-global default currency (the last cascade level). The per-request `config.currency`
    /// takes precedence; this is the fallback resolved inside the blocking closure.
    default_currency: Option<Arc<str>>,
}

/// Runs one invocation to completion on a blocking thread — the shared execute core for `/execute`
/// and each `/batch` item. Wraps the pre-connected `fabricd` session as the egress, runs the
/// invocation under the full-capability profile, then drains the session's driver metrics (the
/// round-trips + drain `block_on` must run on the `spawn_blocking` thread, never a runtime worker).
async fn execute_blocking(
    params: ExecuteBlocking,
) -> Result<(Result<Outcome, EngineError>, EgressMetrics), task::JoinError> {
    let ExecuteBlocking {
        host,
        handle,
        timeout,
        session,
        local_resources,
        local_client,
        source,
        context_json,
        config,
        cache_ns,
        log_floor,
        default_currency,
    } = params;
    task::spawn_blocking(move || -> (Result<Outcome, EngineError>, EgressMetrics) {
        // The broker session (if any) is wrapped as a `SidecarEgress`, then composed with the
        // box-direct bindings into a single `BoxEgress` (D8): a listed local name resolves
        // box-direct, everything else forwards to the broker. The `io` port is wired whenever the
        // request named any resource; a box-direct-only request opened no broker session.
        let broker =
            session.map(|conn| Arc::new(SidecarEgress::new(conn, handle.clone(), timeout)));
        let box_egress = config.io.any().then(|| {
            Arc::new(BoxEgress::new(
                Arc::clone(&local_resources),
                local_client.clone(),
                handle.clone(),
                timeout,
                broker,
            ))
        });
        let egress: Option<Arc<dyn Egress>> = box_egress.as_ref().map(|metered| {
            // Upcast `Arc<BoxEgress>` → `Arc<dyn Egress>`; the turbofish pins the source type so the
            // clone resolves before the coercion (the original `box_egress` stays for draining).
            let dynamic: Arc<dyn Egress> = Arc::<BoxEgress>::clone(metered);
            dynamic
        });
        // The HTTP front always runs the full-capability profile (the default) with no read-hook, so
        // only `caps`, the egress port, and the cache namespace differ from the defaults.
        let enabled_io = config.io.enabled_names();
        let caps = CapabilitySet {
            allowed_hosts: &config.allowed_hosts,
            s3: config.s3.as_ref(),
            sys: config.sys.as_ref(),
            io: &enabled_io,
        };
        let mut invocation = Invocation::inline(source.as_str(), &context_json).caps(caps);
        // Currency cascade (last two levels): per-request `config.currency` wins over the operator
        // default. The explicit `$(amount, currency)` arg (level 1) resolves script-side.
        if let Some(currency) = config.currency.as_deref().or(default_currency.as_deref()) {
            invocation = invocation.default_currency(currency);
        }
        if let Some(port) = egress {
            invocation = invocation.egress(port);
        }
        if let Some(namespace) = cache_ns.as_deref() {
            invocation = invocation.cache_namespace(namespace);
        }
        if let Some(floor) = log_floor {
            invocation = invocation.log_level(floor);
        }
        let outcome = host.run(invocation);
        let metrics = box_egress.map_or_else(EgressMetrics::default, |metered| EgressMetrics {
            backend: metered.drain_metrics(),
            local: metered.drain_local(),
        });
        (outcome, metrics)
    })
    .await
}

// ===== POST /batch — independent per-item fan-out over the single-execute machinery =====

/// A `/batch` request body: an ordered list of independent items plus an optional
/// `before`/`shared`/`after` lifecycle. No atomicity, no cross-item ordering guarantee during
/// execution — only the results array preserves request order.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchRequest {
    /// The items to execute (validated for count/size before any admission).
    #[serde(default)]
    items: Vec<BatchItem>,
    /// Optional one-time setup phase run **alone before any item** (design D1/D2). Its returned
    /// `data`, merged over the `shared` seed, becomes the immutable shared context every item reads.
    /// A `before` failure is a barrier: the whole batch aborts non-200 and no item runs (RQ1/D3).
    #[serde(default)]
    before: Option<BatchItem>,
    /// Optional read-only seed object merged into the shared context (constants that need no fetch).
    /// Absent ⇒ the shared context is `before`'s output alone; both absent ⇒ no shared context is
    /// injected and the batch behaves exactly as today.
    #[serde(default)]
    shared: Option<Box<RawValue>>,
    /// Optional reduce phase run **alone after all items complete** (design D1/D2). It receives the
    /// order-preserving `results` (full per-item envelopes, RQ2); its returned `data` becomes the
    /// batch-level `summary`. An `after` failure is best-effort: HTTP 200 with `results` intact and a
    /// `meta.summary_error` (RQ1/D3).
    #[serde(default)]
    after: Option<BatchItem>,
}

/// One `/batch` item — the single-execute body shape plus an optional client `id` echoed back on its
/// result (D7). No per-item `partition`: fairness is keyed off the request's tenant/partition, shared
/// across all items so a caller cannot split its fairness bucket.
#[derive(Debug, Deserialize)]
struct BatchItem {
    /// Inline JavaScript source (exactly one of `script` / `key`).
    #[serde(default)]
    script: Option<String>,
    /// Registered-script key (exactly one of `script` / `key`).
    #[serde(default)]
    key: Option<String>,
    /// Raw context passed straight to `QuickJS`.
    #[serde(default = "default_context")]
    context: Box<RawValue>,
    /// Per-item configuration (capabilities, `io`).
    #[serde(default)]
    config: RequestConfig,
    /// Optional client correlation id, echoed on the result for subset-retry (D7).
    #[serde(default)]
    id: Option<String>,
}

/// The `/batch` response: order-preserving per-item envelopes + a batch-level summary.
///
/// `summary`/`summary_error` sit at the **top level**, peer to `results` (design RQ1) — the reduced
/// value is a primary product of the batch, not metadata about it. Both are omitted when absent, so
/// an unadorned batch response (no `after` phase) is byte-identical to the pre-lifecycle format.
#[derive(Debug, Serialize)]
struct BatchResponse {
    /// One rendered `{data, error, meta, id?}` envelope per item, in request order.
    results: Vec<Box<RawValue>>,
    /// The `after` phase's reduced value over `results` (design RQ1); omitted when no `after` ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<Box<RawValue>>,
    /// The classified error of a **failed** `after` phase (design RQ1/D3); the batch still responds
    /// `200` with `results` intact. Omitted when `after` was absent or succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_error: Option<ErrorEnvelope>,
    /// Batch-level summary.
    meta: BatchMeta,
}

/// Batch-level summary metadata (per-item timing/metrics live in each `results[i].meta`).
#[derive(Debug, Serialize)]
struct BatchMeta {
    /// Number of items in the batch.
    items: usize,
    /// Items that executed successfully (engine success).
    ok: usize,
    /// Items that failed (rejected, engine error, or truncated).
    failed: usize,
    /// Wall-clock duration of the whole batch fan-out, milliseconds.
    duration_ms: u128,
    /// The batch correlation id — shared by every item's `meta.trace_id`.
    trace_id: String,
}

/// A fully-rendered batch item: its serialized `{data, error, meta, id?}` JSON plus whether it
/// counted as a success (for the batch `meta.ok/failed` summary).
struct RenderedItem {
    /// Serialized item envelope (owned, valid JSON text).
    body: String,
    /// `true` iff the item executed successfully (engine success, not a rejection/error/truncation).
    ok: bool,
}

impl RenderedItem {
    /// Serialized byte length — the unit the D6 total-response cap sums.
    const fn bytes(&self) -> usize {
        self.body.len()
    }
}

/// A serialized system-error item: `{ data: null, error, meta, id? }`.
#[derive(Serialize)]
struct ItemErrorEnvelope<'a> {
    /// Always `null` on a per-item system error.
    data: Option<()>,
    /// The structured error envelope.
    error: &'a ErrorEnvelope,
    /// Per-item metadata.
    meta: &'a Meta,
    /// The echoed client id, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
}

/// A serialized success item: `{ data, error, meta, id? }` (data/error borrowed from the JS output).
#[derive(Serialize)]
struct ItemSuccessEnvelope<'a> {
    /// The JS handler's `data` (zero-copy borrow).
    data: &'a RawValue,
    /// The JS handler's `error` (zero-copy borrow; the application-level error passthrough).
    error: &'a RawValue,
    /// Per-item metadata.
    meta: &'a Meta,
    /// The echoed client id, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
}

/// `POST /batch` — execute a list of independent items, one rendered `{data, error, meta}` envelope
/// each, order-preserving. Batch-level failures (auth, identity, malformed body, caps) use non-200;
/// an admitted batch always returns `200` with per-item envelopes (design D4).
pub(crate) async fn batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<BatchRequest>, JsonRejection>,
) -> AxumResponse {
    let span = build_request_span(&headers);
    run_batch(state, headers, payload).instrument(span).await
}

/// The `/batch` request pipeline: batch-level gates (auth → identity → parse → caps), then the
/// optional three-phase lifecycle — **`before`** (barrier) → the bounded concurrent **items**
/// fan-out (each reading the immutable shared context) → **`after`** (reduce to `summary`) — closed
/// by order-preserving assembly with the D6 response-size cap.
async fn run_batch(
    state: AppState,
    headers: HeaderMap,
    payload: Result<Json<BatchRequest>, JsonRejection>,
) -> AxumResponse {
    // Edge service credential (defense in depth) — reject the whole batch before any body work.
    if let Some(rejected) = enforce_auth(&state, &headers) {
        return rejected;
    }
    let trace_id = current_trace_id();
    // Trusted-identity ingress applies to the whole batch: every item shares this request's tenant.
    let identity = match resolve_identity(&state, &headers, &trace_id) {
        Ok(identity) => identity,
        Err(rejected) => {
            state.metrics.record_rejection();
            return *rejected;
        }
    };
    record_identity_attrs(identity.as_ref());

    let req = match payload {
        Ok(Json(req)) => req,
        Err(rejection) => {
            state.metrics.record_rejection();
            emit_denied(
                &state,
                identity.as_ref(),
                &trace_id,
                "MALFORMED_REQUEST",
                None,
            );
            return malformed_batch_response(&state, &rejection);
        }
    };
    let BatchRequest {
        items,
        before,
        shared,
        after,
    } = req;

    // Batch-level caps (D3), before any item is admitted or executed. The `before`/`after` lifecycle
    // phases do NOT count against `max_items` (RQ3) — only the fan-out width is capped here.
    if let Err(rejected) = validate_batch(&state, &items, identity.as_ref(), &trace_id) {
        state.metrics.record_rejection();
        return *rejected;
    }

    // Shared fairness/partition key (request-level): the trusted tenant in trusted mode, else the
    // caller-asserted header. Every item (and `before`/`after`) keys off this — a batch cannot split
    // its fairness bucket.
    let partition = resolve_partition(identity.as_ref(), header_partition(&headers));
    let env = BatchEnv {
        state: &state,
        identity: identity.as_ref(),
        partition: partition.as_deref(),
        trace_id: &trace_id,
        cfg: state.resp_cfg(),
    };
    let start = Instant::now();
    let count = items.len();

    // ── Phase 1: `before` (barrier) ── run alone, build the immutable shared context, or abort the
    // whole batch non-200 on failure (RQ1/D3). Absent `before` + absent `shared` ⇒ `None` (no
    // injection, byte-identical to the pre-lifecycle path).
    let shared_ctx = match run_before_phase(&env, before, shared.as_deref()).await {
        Ok(built) => built,
        Err(barrier) => {
            state.metrics.record_rejection();
            return barrier;
        }
    };

    // ── Phase 2: items fan-out (unchanged) ── each item reads the immutable shared context.
    let slots = fan_out_items(&env, items, shared_ctx.clone()).await;

    // Render the order-preserving results (enforcing the D6 response-size cap) once — the same array
    // is both the client's `results` and the `after` reducer's input (RQ2).
    let (results, ok, failed) = render_slots(slots, &trace_id, state.batch.max_response_bytes);

    // ── Phase 3: `after` (best-effort reduce) ── runs alone over the full per-item envelopes; its
    // output is the batch `summary`, a failure surfaces as `summary_error` on a 200 (RQ1/D3).
    let (summary, summary_error) =
        run_after_phase(&env, after, &results, shared_ctx.as_deref()).await;

    let duration_ms = start.elapsed().as_millis();
    let meta = BatchMeta {
        items: count,
        ok,
        failed,
        duration_ms,
        trace_id,
    };
    (
        StatusCode::OK,
        Json(BatchResponse {
            results,
            summary,
            summary_error,
            meta,
        }),
    )
        .into_response()
}

/// The shared, borrowed context for the batch lifecycle phases — the request-level state every phase
/// keys off (grouped so the `before`/`after` signatures stay within the argument-count lint).
struct BatchEnv<'a> {
    /// Shared application state.
    state: &'a AppState,
    /// The request's trusted identity (shared across every phase), if any.
    identity: Option<&'a TrustedIdentity>,
    /// The request's fairness/cache key (shared across every phase).
    partition: Option<&'a str>,
    /// The batch correlation id.
    trace_id: &'a str,
    /// Response-shaping policy (drives the `before` barrier's projected status).
    cfg: RespCfg,
}

/// Phase 2 — the bounded concurrent item fan-out (unchanged behavior). Bounds this batch's
/// concurrency to its fair share so it cannot monopolize the pool (D2): the per-partition ceiling
/// when fairness is on, else the global bulkhead capacity. Items beyond the ceiling queue on this
/// gate rather than fast-failing (unlike single `/execute`). Each item reads the immutable shared
/// context. Returns positional slots so the results array preserves request order regardless of
/// completion order (a slot left empty by a panicked task is filled defensively downstream).
async fn fan_out_items(
    env: &BatchEnv<'_>,
    items: Vec<BatchItem>,
    shared_ctx: Option<Arc<str>>,
) -> Vec<Option<RenderedItem>> {
    let count = items.len();
    let gate = Arc::new(Semaphore::new(batch_ceiling(env.state)));
    let mut set: JoinSet<(usize, RenderedItem)> = JoinSet::new();
    for (index, item) in items.into_iter().enumerate() {
        let task_state = env.state.clone();
        let task_gate = Arc::clone(&gate);
        let task_identity = env.identity.cloned();
        let task_partition = env.partition.map(str::to_owned);
        let task_trace = env.trace_id.to_owned();
        let task_shared = shared_ctx.clone();
        let _abort = set.spawn(async move {
            let rendered = run_batch_item(BatchItemCtx {
                state: &task_state,
                gate: task_gate,
                identity: task_identity.as_ref(),
                partition: task_partition.as_deref(),
                trace_id: &task_trace,
                item,
                shared: task_shared,
            })
            .await;
            (index, rendered)
        });
    }

    let mut slots: Vec<Option<RenderedItem>> = (0..count).map(|_idx| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((index, rendered)) = joined
            && let Some(slot) = slots.get_mut(index)
        {
            *slot = Some(rendered);
        }
    }
    slots
}

/// This batch's concurrency ceiling: the per-partition fair share when fairness is enabled, else the
/// global bulkhead capacity. Never exceeds the bulkhead capacity, always ≥1.
fn batch_ceiling(state: &AppState) -> usize {
    let per_partition = state.engine_cfg.max_concurrent_per_partition;
    let ceiling = if per_partition > 0 {
        per_partition
    } else {
        state.bulkhead_capacity
    };
    ceiling.min(state.bulkhead_capacity).max(1)
}

/// Batch-level caps enforced before any admission (D3): non-empty, within `max_items`, and combined
/// input bytes within `max_input_bytes`. Returns the ready `400` response on violation, emitting the
/// denied audit at each gate.
fn validate_batch(
    state: &AppState,
    items: &[BatchItem],
    identity: Option<&TrustedIdentity>,
    trace_id: &str,
) -> Result<(), Box<AxumResponse>> {
    if items.is_empty() {
        emit_denied(state, identity, trace_id, "EMPTY_BATCH", None);
        return Err(Box::new(batch_level_error(
            "EMPTY_BATCH",
            "batch must contain at least one item".to_owned(),
            trace_id,
        )));
    }
    if items.len() > state.batch.max_items {
        emit_denied(state, identity, trace_id, "BATCH_TOO_LARGE", None);
        return Err(Box::new(batch_level_error(
            "BATCH_TOO_LARGE",
            format!(
                "batch has {} items, exceeding the limit of {}",
                items.len(),
                state.batch.max_items
            ),
            trace_id,
        )));
    }
    let combined = items
        .iter()
        .map(batch_item_input_bytes)
        .fold(0_usize, usize::saturating_add);
    if combined > state.batch.max_input_bytes {
        emit_denied(state, identity, trace_id, "BATCH_INPUT_TOO_LARGE", None);
        return Err(Box::new(batch_level_error(
            "BATCH_INPUT_TOO_LARGE",
            format!(
                "combined input {combined} bytes exceeds the limit of {}",
                state.batch.max_input_bytes
            ),
            trace_id,
        )));
    }
    Ok(())
}

/// One item's request-body input size: inline script length (0 for a `key` — the registered source
/// is resolved and sized per item) plus context length. Sums to the combined-input cap.
fn batch_item_input_bytes(item: &BatchItem) -> usize {
    let script = item.script.as_ref().map_or(0, String::len);
    script.saturating_add(item.context.get().len())
}

/// A batch-level `400` response (malformed body / caps): the single `{data:null, error, meta}`
/// envelope with the batch trace id — batch-level rejections are non-200 (design D4).
fn batch_level_error(code: &str, message: String, trace_id: &str) -> AxumResponse {
    system_error_response(
        request_error(code, message),
        400,
        Meta::new(trace_id.to_owned(), 0, 0, 0),
    )
}

/// The structured `400` for a `/batch` body that failed to parse (bad JSON / wrong types / oversize).
fn malformed_batch_response(state: &AppState, rejection: &JsonRejection) -> AxumResponse {
    let trace_id = Uuid::new_v4().to_string();
    let base = request_error(
        "MALFORMED_REQUEST",
        "request body is not valid for /batch".to_owned(),
    );
    let envelope = if state.error_debug {
        base.with_debug(ErrorDebug {
            stack: None,
            raw: Some(rejection.body_text()),
        })
    } else {
        base
    };
    system_error_response(envelope, 400, Meta::new(trace_id, 0, 0, 0))
}

/// Inputs for running one batch item (grouped to stay within the argument-count lint).
struct BatchItemCtx<'a> {
    /// Shared application state.
    state: &'a AppState,
    /// This batch's fair-share concurrency gate.
    gate: Arc<Semaphore>,
    /// The request's trusted identity (shared across items), if any.
    identity: Option<&'a TrustedIdentity>,
    /// The request's fairness/cache key (shared across items).
    partition: Option<&'a str>,
    /// The batch correlation id (every item's `meta.trace_id`).
    trace_id: &'a str,
    /// The item to run.
    item: BatchItem,
    /// The immutable shared context built by the `before` phase (serialized JSON), or `None` when the
    /// batch has no lifecycle. When present it is injected read-only into the item's context object
    /// under the reserved `shared` key (design D4/RQ3); the `Arc` is shared, never cloned per item.
    shared: Option<Arc<str>>,
}

/// Runs one batch item through the same per-request machinery as `/execute`, rendering a per-item
/// envelope instead of an HTTP status: resolve script → validate size → per-item authz (D5) →
/// per-item quota debit (D5) → open session → admit (queue) → execute → render. Every security gate
/// is the exact function `/execute` uses, evaluated per item — never once for the batch.
#[expect(
    clippy::too_many_lines,
    reason = "linear per-item pipeline mirroring run_execute: resolve → validate → authz → quota → session → admit → execute → render"
)]
async fn run_batch_item(ctx: BatchItemCtx<'_>) -> RenderedItem {
    let BatchItemCtx {
        state,
        gate,
        identity,
        partition,
        trace_id,
        item,
        shared,
    } = ctx;
    let BatchItem {
        script,
        key,
        context,
        config,
        id,
    } = item;
    let id_ref = id.as_deref();
    let context_bytes = context.get().len();

    // Resolve exactly one of script/key.
    let source = match resolve_script(script, key.as_deref(), &state.registry) {
        Ok(source) => source,
        Err(boxed) => {
            let (_status, envelope) = *boxed;
            emit_denied(state, identity, trace_id, "SCRIPT_NOT_FOUND", None);
            let meta = base_error_meta(trace_id, 0, context_bytes, key.as_deref(), partition);
            return render_error_item(&envelope, &meta, id_ref);
        }
    };
    let script_bytes = source.as_str().len();

    // Per-item input-size validation.
    if let Err((code, message)) = sandbox::validate_input_sizes(
        script_bytes,
        context_bytes,
        state.engine_cfg.max_script_size,
        state.engine_cfg.max_context_size,
    ) {
        emit_denied(state, identity, trace_id, "INPUT_TOO_LARGE", None);
        let meta = base_error_meta(
            trace_id,
            script_bytes,
            context_bytes,
            key.as_deref(),
            partition,
        );
        return render_error_item(&request_error(code, message), &meta, id_ref);
    }

    // Inject the immutable shared context (when the batch has a `before`/`shared` lifecycle) into the
    // item's context object under the reserved `shared` key (design D4/RQ3). Requires an object
    // context (the default and near-universal shape); a non-object context alongside a shared context
    // is a per-item caller error. No shared context ⇒ the item's context passes through verbatim.
    let context_json = if let Some(shared_json) = shared.as_deref() {
        let Some(merged) = context_with_reserved(&context, &[("shared", shared_json)]) else {
            emit_denied(state, identity, trace_id, "INVALID_CONTEXT", None);
            let meta =
                base_error_meta(trace_id, script_bytes, context_bytes, key.as_deref(), partition);
            let envelope = request_error(
                "INVALID_CONTEXT",
                "item context must be a JSON object when the batch supplies a shared context"
                    .to_owned(),
            );
            return render_error_item(&envelope, &meta, id_ref);
        };
        merged
    } else {
        context.get().to_owned()
    };

    // Per-item member-capability authz (trusted mode, D5) — evaluated for EVERY item, never once for
    // the batch (the GraphQL-batch-attack guard).
    if let Some(envelope) = batch_item_authz(state, identity, &config, trace_id) {
        let meta = base_error_meta(
            trace_id,
            script_bytes,
            context_bytes,
            key.as_deref(),
            partition,
        );
        return render_error_item(&envelope, &meta, id_ref);
    }

    // Per-item quota debit (trusted mode) — held across this item's execution (D5: counts N, not 1).
    let _item_quota = match batch_item_quota(state, identity, trace_id) {
        Ok(guard) => guard,
        Err(envelope) => {
            let meta = base_error_meta(
                trace_id,
                script_bytes,
                context_bytes,
                key.as_deref(),
                partition,
            );
            return render_error_item(&envelope, &meta, id_ref);
        }
    };

    // Open the fabricd session only for broker-resolved names (box-direct names are served locally).
    let broker_names = config.io.broker_names(&state.local_resources);
    let session = if broker_names.is_empty() {
        None
    } else {
        let tenant = identity.and_then(|trusted| trusted.tenant.as_deref());
        let init = wire_init(broker_names, state.engine_cfg.timeout(), tenant);
        match connect_session(&state.transport, &init).await {
            Ok(conn) => Some(conn),
            Err(err) => {
                emit_denied(state, identity, trace_id, "EGRESS_UNAVAILABLE", None);
                let envelope = session_error_envelope(err);
                let meta = base_error_meta(
                    trace_id,
                    script_bytes,
                    context_bytes,
                    key.as_deref(),
                    partition,
                );
                return render_error_item(&envelope, &meta, id_ref);
            }
        }
    };

    // Admit: acquire this batch's fair-share slot (queues past the ceiling) then a global bulkhead
    // permit (queues; protects blocking threads). Both held across execution.
    let _slot = gate.acquire_owned().await.ok();
    let _global = Arc::clone(&state.limiter).acquire_owned().await.ok();

    let start = Instant::now();
    let result = execute_blocking(ExecuteBlocking {
        host: state.host.clone(),
        handle: Handle::current(),
        timeout: state.engine_cfg.timeout(),
        session,
        local_resources: Arc::clone(&state.local_resources),
        local_client: state.local_client.clone(),
        source,
        context_json,
        config,
        cache_ns: partition.map(str::to_owned),
        // Batch items neither mirror nor stream logs (out of scope for §3); use the host's floor.
        log_floor: None,
        default_currency: state.default_currency.clone(),
    })
    .await;

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = Meta::new(
        trace_id.to_owned(),
        script_bytes,
        context_bytes,
        exec_time_us,
    )
    .with_key(key)
    .with_partition(partition.map(str::to_owned));
    render_executed_item(state, identity, result, base_meta, id_ref)
}

/// Per-item member-capability authz (trusted mode, D5). `None` = permitted (or not gated); `Some`
/// carries the `ENTITLEMENT_REQUIRED` envelope. Emits the denied audit on rejection.
fn batch_item_authz(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    config: &RequestConfig,
    trace_id: &str,
) -> Option<ErrorEnvelope> {
    let (Some(trusted), Some(id)) = (state.trusted.as_ref(), identity) else {
        return None;
    };
    let requested = requested_capabilities(config);
    match authorize_capabilities(&trusted.capability_entitlements, &requested, id) {
        Ok(()) => None,
        Err(denied) => {
            emit_denied(
                state,
                identity,
                trace_id,
                "ENTITLEMENT_REQUIRED",
                Some(json!({ "capability": denied.capability, "required": denied.required })),
            );
            let message = format!(
                "capability `{}` requires entitlement `{}`",
                denied.capability, denied.required
            );
            Some(request_error("ENTITLEMENT_REQUIRED", message))
        }
    }
}

/// Per-item quota debit (trusted mode). Returns the in-flight guard (held across the item's
/// execution) or the `QUOTA_EXCEEDED` envelope. Emits the denied audit on rejection. This is what
/// makes a batch cost N quota units, not 1 (D5).
fn batch_item_quota(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    trace_id: &str,
) -> Result<Option<QuotaGuard>, Box<ErrorEnvelope>> {
    let (Some(trusted), Some(id)) = (state.trusted.as_ref(), identity) else {
        return Ok(None);
    };
    let Some(quota) = trusted.quota.as_ref() else {
        return Ok(None);
    };
    let Some(tenant) = id.tenant.as_deref() else {
        return Ok(None);
    };
    match quota.admit(tenant, id.plan.as_deref()) {
        Ok(guard) => Ok(Some(guard)),
        Err(exceeded) => {
            emit_denied(
                state,
                identity,
                trace_id,
                "QUOTA_EXCEEDED",
                Some(json!({
                    "plan": exceeded.plan,
                    "limit": exceeded.limit,
                    "usage": exceeded.usage,
                })),
            );
            Err(Box::new(quota_exceeded_envelope(&exceeded)))
        }
    }
}

/// Renders one item's execution outcome (mirrors [`build_response`]): emits the per-item usage/audit
/// events + records per-item metrics, then serializes the `{data, error, meta, id?}` envelope.
fn render_executed_item(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    result: Result<(Result<Outcome, EngineError>, EgressMetrics), task::JoinError>,
    base_meta: Meta,
    id: Option<&str>,
) -> RenderedItem {
    let metrics: &Metrics = &state.metrics;
    metrics.observe_execution(base_meta.exec_time_us);
    match result {
        Ok((Ok(exec), egress)) => {
            record_capability_latencies(metrics, &exec.metrics, &egress.backend);
            let meta = base_meta.with_metrics(exec.metrics, egress);
            match exec.result {
                ExecOutcome::Success(js_json) => {
                    emit_executed(state, identity, &meta, "success");
                    metrics.record_success();
                    render_success_item(&js_json, &meta, id, state.resp_cfg())
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome);
                    metrics.record_engine_error(&engine_err);
                    render_engine_error_item(engine_err, &meta, id, state.resp_cfg())
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            render_engine_error_item(engine_err, &base_meta, id, state.resp_cfg())
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            render_engine_error_item(engine_err, &base_meta, id, state.resp_cfg())
        }
    }
}

/// Serializes a success item from the JS `{data, error}` output + meta + id. A JS output that does
/// not parse is rendered as a `MALFORMED_RESPONSE` error item instead (mirrors [`success_response`]).
fn render_success_item(js_json: &str, meta: &Meta, id: Option<&str>, cfg: RespCfg) -> RenderedItem {
    match serde_json::from_str::<Envelope<'_>>(js_json) {
        Ok(env) => {
            let envelope = ItemSuccessEnvelope {
                data: env.data,
                error: env.error,
                meta,
                id,
            };
            let body = serde_json::to_string(&envelope)
                .unwrap_or_else(|_err| fallback_item_body(&meta.trace_id));
            RenderedItem { body, ok: true }
        }
        Err(parse_err) => {
            let envelope =
                EngineError::Malformed(format!("malformed handler response: {parse_err}"))
                    .into_envelope(cfg.error_debug, cfg.timeout_retryable);
            render_error_item(&envelope, meta, id)
        }
    }
}

/// Serializes an engine-error item, logging the raw cause server-side keyed by `trace_id` (mirrors
/// [`engine_error_response`]). A `/batch` item's classification rides its rendered envelope inside
/// the `200` batch (design D4) — the projected HTTP status applies to single `/execute` only.
fn render_engine_error_item(
    err: EngineError,
    meta: &Meta,
    id: Option<&str>,
    cfg: RespCfg,
) -> RenderedItem {
    warn!(trace_id = %meta.trace_id, error = ?err, "batch item system error");
    let envelope = err.into_envelope(cfg.error_debug, cfg.timeout_retryable);
    render_error_item(&envelope, meta, id)
}

/// Serializes a system-error item: `{ data: null, error, meta, id? }`.
fn render_error_item(error: &ErrorEnvelope, meta: &Meta, id: Option<&str>) -> RenderedItem {
    let envelope = ItemErrorEnvelope {
        data: None,
        error,
        meta,
        id,
    };
    let body =
        serde_json::to_string(&envelope).unwrap_or_else(|_err| fallback_item_body(&meta.trace_id));
    RenderedItem { body, ok: false }
}

/// A minimal valid item body used only if serializing an item envelope somehow fails (unreachable in
/// practice — the fields are plain JSON).
fn fallback_item_body(trace_id: &str) -> String {
    format!(
        "{{\"data\":null,\"error\":{{\"code\":\"INTERNAL_ERROR\"}},\"meta\":{{\"trace_id\":{trace_id:?}}}}}"
    )
}

// ===== Batch lifecycle: before → items → after (design D1/D2, RQ1–RQ3) =====

/// Merges framework-supplied read-only `reserved` keys into a JSON **object** `context`, returning the
/// serialized result. Values are kept as `RawValue` so number fidelity survives the round-trip; a
/// reserved key overwrites a same-named field the caller declared (the framework value wins). Returns
/// `None` when `context` is not a JSON object (the caller decides how to surface that).
fn context_with_reserved(context: &RawValue, reserved: &[(&str, &str)]) -> Option<String> {
    let mut map: BTreeMap<String, Box<RawValue>> = serde_json::from_str(context.get()).ok()?;
    for &(key, value) in reserved {
        let _prev = map.insert(key.to_owned(), RawValue::from_string(value.to_owned()).ok()?);
    }
    serde_json::to_string(&map).ok()
}

/// Builds the immutable shared context from the `shared` seed and `before`'s returned `data` (design
/// D4/RQ3). `None` ⇒ neither was supplied (no injection, byte-identical to the pre-lifecycle path).
/// When both are JSON objects they shallow-merge with `before`'s data winning; otherwise `before`'s
/// data (when present) is the shared context verbatim and the seed is ignored.
fn build_shared_context(seed: Option<&RawValue>, before_data: Option<&RawValue>) -> Option<String> {
    match (seed, before_data) {
        (None, None) => None,
        (Some(seed_only), None) => Some(seed_only.get().to_owned()),
        (None, Some(data)) => Some(data.get().to_owned()),
        (Some(seed_obj), Some(data)) => Some(
            match (
                serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(seed_obj.get()),
                serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(data.get()),
            ) {
                (Ok(mut merged), Ok(over)) => {
                    merged.extend(over);
                    serde_json::to_string(&merged).unwrap_or_else(|_err| data.get().to_owned())
                }
                _ => data.get().to_owned(),
            },
        ),
    }
}

/// The outcome of a `before`/`after` lifecycle invocation: the extracted handler `data` on success
/// (used to build the shared context / the `summary`), or a classified error (a gate rejection or an
/// engine error) that becomes the `before` barrier response or the `after` `summary_error`.
enum LifecyclePhase {
    /// Handler succeeded; carries its returned `data` (the reproducible product, RQ2).
    Success(Box<RawValue>),
    /// A gate rejection or engine error, already classified into a wire envelope.
    Failure(ErrorEnvelope),
}

/// Inputs for one lifecycle invocation (grouped to stay within the argument-count lint). Mirrors
/// [`BatchItemCtx`] but carries the already-merged `context_json` (the phase's own context for
/// `before`; the `results`/`shared` reserved keys merged in for `after`) and returns a
/// [`LifecyclePhase`] rather than a rendered envelope.
struct LifecycleCtx<'a> {
    /// Shared application state.
    state: &'a AppState,
    /// The request's trusted identity (shared with the items), if any.
    identity: Option<&'a TrustedIdentity>,
    /// The request's fairness/cache key (shared with the items).
    partition: Option<&'a str>,
    /// The batch correlation id.
    trace_id: &'a str,
    /// The phase invocation (script/key/config; its `context` field is superseded by `context_json`).
    item: BatchItem,
    /// The fully-merged context handed to the handler.
    context_json: String,
}

/// Runs one `before`/`after` phase through the **same per-invocation gates an item gets** (resolve →
/// size → authz → quota debit → session → admit → execute), returning the structured outcome (design
/// D2). A lifecycle phase is never a cheaper unit of admission/quota/billing than an item; it runs
/// alone (sequentially, outside the fan-out) so it needs no fair-share slot, only the global bulkhead
/// permit that protects the blocking threads.
async fn run_lifecycle_phase(ctx: LifecycleCtx<'_>) -> LifecyclePhase {
    let LifecycleCtx {
        state,
        identity,
        partition,
        trace_id,
        item,
        context_json,
    } = ctx;
    let BatchItem {
        script,
        key,
        context: _superseded,
        config,
        id: _unused,
    } = item;
    let context_bytes = context_json.len();

    let source = match resolve_script(script, key.as_deref(), &state.registry) {
        Ok(source) => source,
        Err(boxed) => {
            let (_status, envelope) = *boxed;
            emit_denied(state, identity, trace_id, "SCRIPT_NOT_FOUND", None);
            return LifecyclePhase::Failure(envelope);
        }
    };
    let script_bytes = source.as_str().len();

    if let Err((code, message)) = sandbox::validate_input_sizes(
        script_bytes,
        context_bytes,
        state.engine_cfg.max_script_size,
        state.engine_cfg.max_context_size,
    ) {
        emit_denied(state, identity, trace_id, "INPUT_TOO_LARGE", None);
        return LifecyclePhase::Failure(request_error(code, message));
    }

    // Same per-invocation member-capability authz + quota debit an item gets (RQ3): a lifecycle phase
    // counts against quota, so a batch with a `before`/`after` is never cheaper than the equivalent
    // single requests. The quota guard is held across this phase's execution.
    if let Some(envelope) = batch_item_authz(state, identity, &config, trace_id) {
        return LifecyclePhase::Failure(envelope);
    }
    let _quota = match batch_item_quota(state, identity, trace_id) {
        Ok(guard) => guard,
        Err(envelope) => return LifecyclePhase::Failure(*envelope),
    };

    let broker_names = config.io.broker_names(&state.local_resources);
    let session = if broker_names.is_empty() {
        None
    } else {
        let tenant = identity.and_then(|trusted| trusted.tenant.as_deref());
        let init = wire_init(broker_names, state.engine_cfg.timeout(), tenant);
        match connect_session(&state.transport, &init).await {
            Ok(conn) => Some(conn),
            Err(err) => {
                emit_denied(state, identity, trace_id, "EGRESS_UNAVAILABLE", None);
                return LifecyclePhase::Failure(session_error_envelope(err));
            }
        }
    };

    // A lifecycle phase runs alone; acquire only the global bulkhead permit (protects blocking
    // threads), not the batch's fair-share gate.
    let _global = Arc::clone(&state.limiter).acquire_owned().await.ok();

    let start = Instant::now();
    let result = execute_blocking(ExecuteBlocking {
        host: state.host.clone(),
        handle: Handle::current(),
        timeout: state.engine_cfg.timeout(),
        session,
        local_resources: Arc::clone(&state.local_resources),
        local_client: state.local_client.clone(),
        source,
        context_json,
        config,
        cache_ns: partition.map(str::to_owned),
        log_floor: None,
        default_currency: state.default_currency.clone(),
    })
    .await;

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = Meta::new(trace_id.to_owned(), script_bytes, context_bytes, exec_time_us)
        .with_partition(partition.map(str::to_owned));
    lifecycle_outcome(state, identity, result, base_meta)
}

/// Classifies a lifecycle phase's execution outcome (mirrors [`render_executed_item`] but extracts the
/// handler `data` instead of rendering an envelope): emits the per-invocation usage/audit events +
/// records metrics on every path, then yields `Success(data)` or `Failure(envelope)`.
fn lifecycle_outcome(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    result: Result<(Result<Outcome, EngineError>, EgressMetrics), task::JoinError>,
    base_meta: Meta,
) -> LifecyclePhase {
    let metrics: &Metrics = &state.metrics;
    let cfg = state.resp_cfg();
    metrics.observe_execution(base_meta.exec_time_us);
    match result {
        Ok((Ok(exec), egress)) => {
            record_capability_latencies(metrics, &exec.metrics, &egress.backend);
            let meta = base_meta.with_metrics(exec.metrics, egress);
            match exec.result {
                ExecOutcome::Success(js_json) => {
                    emit_executed(state, identity, &meta, "success");
                    metrics.record_success();
                    match serde_json::from_str::<Envelope<'_>>(&js_json) {
                        Ok(env) => LifecyclePhase::Success(
                            RawValue::from_string(env.data.get().to_owned())
                                .unwrap_or_else(|_err| RAW_NULL.clone()),
                        ),
                        Err(parse_err) => LifecyclePhase::Failure(
                            EngineError::Malformed(format!("malformed handler response: {parse_err}"))
                                .into_envelope(cfg.error_debug, cfg.timeout_retryable),
                        ),
                    }
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome);
                    metrics.record_engine_error(&engine_err);
                    LifecyclePhase::Failure(
                        engine_err.into_envelope(cfg.error_debug, cfg.timeout_retryable),
                    )
                }
            }
        }
        Ok((Err(engine_err), _egress)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            LifecyclePhase::Failure(engine_err.into_envelope(cfg.error_debug, cfg.timeout_retryable))
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            LifecyclePhase::Failure(engine_err.into_envelope(cfg.error_debug, cfg.timeout_retryable))
        }
    }
}

/// Phase 1 — the `before` barrier. Runs `before` (when present) alone, then builds the immutable
/// shared context from the `shared` seed + `before`'s `data`, enforcing the `max_shared_bytes` cap.
/// Returns the shared context (`None` ⇒ no lifecycle, inject nothing). Any `before` failure — or an
/// over-cap shared context — becomes a non-200 batch-level barrier response (RQ1/D3): no item runs.
async fn run_before_phase(
    env: &BatchEnv<'_>,
    before: Option<BatchItem>,
    shared: Option<&RawValue>,
) -> Result<Option<Arc<str>>, AxumResponse> {
    let before_data: Option<Box<RawValue>> = match before {
        None => None,
        Some(item) => {
            let context_json = item.context.get().to_owned();
            match run_lifecycle_phase(LifecycleCtx {
                state: env.state,
                identity: env.identity,
                partition: env.partition,
                trace_id: env.trace_id,
                item,
                context_json,
            })
            .await
            {
                LifecyclePhase::Success(data) => Some(data),
                LifecyclePhase::Failure(envelope) => {
                    let meta = base_error_meta(env.trace_id, 0, 0, None, env.partition);
                    return Err(projected_error_response(envelope, meta, env.cfg));
                }
            }
        }
    };

    let Some(json) = build_shared_context(shared, before_data.as_deref()) else {
        return Ok(None);
    };
    if json.len() > env.state.batch.max_shared_bytes {
        emit_denied(
            env.state,
            env.identity,
            env.trace_id,
            "SHARED_CONTEXT_TOO_LARGE",
            None,
        );
        let envelope = request_error(
            "SHARED_CONTEXT_TOO_LARGE",
            format!(
                "shared context {} bytes exceeds the limit of {}",
                json.len(),
                env.state.batch.max_shared_bytes
            ),
        );
        let meta = base_error_meta(env.trace_id, 0, 0, None, env.partition);
        return Err(projected_error_response(envelope, meta, env.cfg));
    }
    Ok(Some(Arc::from(json.as_str())))
}

/// Phase 3 — the best-effort `after` reduce. Runs `after` (when present) alone over the full per-item
/// envelopes (RQ2) plus the shared context, both injected as reserved keys on `after`'s own context.
/// Returns `(summary, summary_error)`: on success the reduced `data` is the `summary`; any failure is
/// surfaced as `summary_error` on a 200 with `results` intact (RQ1/D3) — it never fails the batch.
async fn run_after_phase(
    env: &BatchEnv<'_>,
    after: Option<BatchItem>,
    results: &[Box<RawValue>],
    shared: Option<&str>,
) -> (Option<Box<RawValue>>, Option<ErrorEnvelope>) {
    // `env.cfg` is unused here: an `after` failure never projects an HTTP status — it rides the 200
    // response as `summary_error`.
    let Some(item) = after else {
        return (None, None);
    };

    let results_json = serde_json::to_string(results).unwrap_or_else(|_err| "[]".to_owned());
    let mut reserved: Vec<(&str, &str)> = vec![("results", results_json.as_str())];
    if let Some(shared_json) = shared {
        reserved.push(("shared", shared_json));
    }
    let Some(context_json) = context_with_reserved(&item.context, &reserved) else {
        return (
            None,
            Some(request_error(
                "INVALID_CONTEXT",
                "after context must be a JSON object".to_owned(),
            )),
        );
    };

    match run_lifecycle_phase(LifecycleCtx {
        state: env.state,
        identity: env.identity,
        partition: env.partition,
        trace_id: env.trace_id,
        item,
        context_json,
    })
    .await
    {
        LifecyclePhase::Success(data) => (Some(data), None),
        LifecyclePhase::Failure(envelope) => (None, Some(envelope)),
    }
}

/// Renders the fan-out slots into the order-preserving `results` array, enforcing the total-response-
/// bytes cap (D6): an item whose bytes would push the running total past the cap is truncated to a
/// classified size-limit error envelope rather than buffered. Returns `(results, ok, failed)` — the
/// same array feeds both the client response and the `after` reducer (RQ2), so it is built once.
fn render_slots(
    slots: Vec<Option<RenderedItem>>,
    trace_id: &str,
    max_response_bytes: usize,
) -> (Vec<Box<RawValue>>, usize, usize) {
    let count = slots.len();
    let mut results: Vec<Box<RawValue>> = Vec::with_capacity(count);
    let mut ok = 0_usize;
    let mut failed = 0_usize;
    let mut used = 0_usize;
    for slot in slots {
        let rendered = slot.unwrap_or_else(|| internal_error_item(trace_id));
        let projected = used.saturating_add(rendered.bytes());
        let item = if projected > max_response_bytes {
            let truncated = truncated_item(trace_id);
            used = used.saturating_add(truncated.bytes());
            failed = failed.saturating_add(1);
            truncated
        } else {
            used = projected;
            if rendered.ok {
                ok = ok.saturating_add(1);
            } else {
                failed = failed.saturating_add(1);
            }
            rendered
        };
        let raw = RawValue::from_string(item.body).unwrap_or_else(|_err| RAW_NULL.clone());
        results.push(raw);
    }
    (results, ok, failed)
}

/// A defensive per-item envelope for a slot that never completed (a panicked fan-out task) — a
/// retryable internal error, so a batch never returns a hole in its results array.
fn internal_error_item(trace_id: &str) -> RenderedItem {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "INTERNAL_ERROR".to_owned(),
        true,
        ErrorOwner::Operator,
    )
    .with_message("batch item did not complete".to_owned());
    render_error_item(&envelope, &Meta::new(trace_id.to_owned(), 0, 0, 0), None)
}

/// The classified size-limit envelope an item is truncated to when it would exceed the total-
/// response-bytes cap (D6). Small and fixed so it never itself blows the cap.
fn truncated_item(trace_id: &str) -> RenderedItem {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "BATCH_RESPONSE_TRUNCATED".to_owned(),
        false,
        ErrorOwner::Operator,
    )
    .with_message("item output omitted: batch response size limit reached".to_owned());
    render_error_item(&envelope, &Meta::new(trace_id.to_owned(), 0, 0, 0), None)
}

/// `GET /metrics` — Prometheus text exposition of the process-wide counters and live
/// gauges (bulkhead permits read off the semaphore).
pub(crate) async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let available = state.limiter.available_permits();
    // The db circuit breaker moved to `fabricd` (it owns the driver connections now); the box
    // reports zero trips, keeping the `runlet_db_breaker_trips_total` series present when the breaker is off.
    let trips = 0_u64;
    let cache = state.host.bytecode_cache_stats();
    let mut body = state
        .metrics
        .render(available, state.bulkhead_capacity, trips, cache);
    // Event-pipeline backpressure gauge (Change C): appended here since the counter lives in
    // `runlet` (the event sink), not the `runlet-core` metrics registry. Absent series ⇒ 0.
    if let Some(counter) = state.event_dropped.as_ref() {
        let dropped = counter.load(Ordering::Relaxed);
        body = format!(
            "{body}# HELP runlet_events_dropped_total Usage/audit events dropped due to a full buffer.\n\
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
fn identity_fields(
    identity: Option<&TrustedIdentity>,
) -> (Option<String>, Option<String>, Option<String>) {
    identity.map_or((None, None, None), |id| {
        (id.tenant.clone(), id.user.clone(), id.plan.clone())
    })
}

/// Emits a `usage` event plus an `allowed` audit event for an executed request (Change C). A no-op
/// when event emission is disabled. Every request that reaches execution produces exactly these two.
fn emit_executed(state: &AppState, identity: Option<&TrustedIdentity>, meta: &Meta, outcome: &str) {
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
    ));
    let audit = EventBody::Audit(AuditBody {
        decision: "allowed",
        reason: None,
        detail: None,
    });
    sink.record(Event::new(tenant, user, plan, meta.trace_id.clone(), audit));
}

/// Emits a `denied` audit event carrying the reject reason code (and optional detail) at a gate.
/// A no-op when event emission is disabled.
fn emit_denied(
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
    sink.record(Event::new(tenant, user, plan, trace_id.to_owned(), audit));
}

/// The gateway-asserted diagnostic-log routing policy for a request (§3): whether to mirror the
/// captured logs on the response, and whether the run streams to the live tenant stream (§2). Both
/// derive **only** from trusted signals resolved in the identity layer — never a caller body field.
#[derive(Debug, Clone, Copy)]
struct LogPolicy {
    /// Attach the top-level `logs` list to the response (D5, the playground mirror).
    capture: bool,
    /// The execution mode (OQ1): a `Test`/playground run is response-mirror-only and MUST NOT enter
    /// the live stream / billing / audit.
    mode: RunMode,
}

impl LogPolicy {
    /// Resolves the policy from the request's trusted identity. Outside trusted mode there is no
    /// gateway, so capture is off and the mode is live (a caller can neither force capture nor pick
    /// the mode).
    fn resolve(identity: Option<&TrustedIdentity>) -> Self {
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
fn build_response(
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
                    emit_executed(state, identity, &meta, "success");
                    metrics.record_success();
                    record_span_outcome("success");
                    success_response(&js_json, meta, &effects, mirror, cfg)
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome);
                    metrics.record_engine_error(&engine_err);
                    record_span_outcome(outcome);
                    engine_error_response(engine_err, meta, &effects, mirror, cfg)
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            record_span_outcome(outcome);
            // No Outcome ⇒ no captured logs; when capture was requested, present an empty list.
            let mirror = policy.capture.then_some::<&[LogEntry]>(&[]);
            engine_error_response(engine_err, base_meta, &[], mirror, cfg)
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
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
fn stream_logs(
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
fn record_capability_latencies(metrics: &Metrics, exec: &ExecMetrics, backend: &BackendMetrics) {
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

/// Resolves the script source for a request: exactly one of `script` / `key` must be
/// present; a `key` is looked up in the registry.
///
/// # Errors
///
/// Returns the HTTP status + envelope for the violation (boxed — the happy path
/// shouldn't carry the envelope's size): 400 `SCRIPT_XOR_KEY` when not exactly one of
/// the two is present, 404 `SCRIPT_NOT_FOUND` for an unknown key.
fn resolve_script(
    script: Option<String>,
    key: Option<&str>,
    registry: &ScriptRegistry,
) -> Result<ScriptSource, Box<(u16, ErrorEnvelope)>> {
    match (script, key) {
        (Some(source), None) => Ok(ScriptSource::Inline(source)),
        (None, Some(requested)) => registry
            .get(requested)
            .map(ScriptSource::Registered)
            .ok_or_else(|| {
                Box::new((
                    404,
                    request_error(
                        "SCRIPT_NOT_FOUND",
                        format!("no registered script for key `{requested}`"),
                    ),
                ))
            }),
        (Some(_), Some(_)) | (None, None) => Err(Box::new((
            400,
            request_error(
                "SCRIPT_XOR_KEY",
                "request must include exactly one of `script` or `key`".to_owned(),
            ),
        ))),
    }
}

/// Builds the structured response for a request body that failed to parse or extract
/// (bad JSON, wrong field types, oversized body). Returns the same `{data, error, meta}`
/// envelope as every other error path — never axum's default plain-text rejection — so
/// a client that always parses the envelope never has to special-case malformed input.
/// The rejection's own text is surfaced only in gated `debug.raw`.
fn malformed_request_response(state: &AppState, rejection: &JsonRejection) -> AxumResponse {
    let trace_id = Uuid::new_v4().to_string();
    let base = request_error(
        "MALFORMED_REQUEST",
        "request body is not valid for /execute".to_owned(),
    );
    let envelope = if state.error_debug {
        base.with_debug(ErrorDebug {
            stack: None,
            raw: Some(rejection.body_text()),
        })
    } else {
        base
    };
    system_error_response(envelope, 400, Meta::new(trace_id, 0, 0, 0))
}

/// Builds the `OVERLOADED` response when the bulkhead is saturated: a runtime-category envelope,
/// retryable, owned by the operator (capacity, not the caller's request). Retryable ⇒ projects to
/// `503` + `Retry-After` (**never `429`** — a `4xx` digit would make a status-line worker park a
/// response that only needs to wait; the `Retry-After` value carries the backoff horizon).
fn overloaded_response(meta: Meta, cfg: RespCfg) -> AxumResponse {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "OVERLOADED".to_owned(),
        true,
        ErrorOwner::Operator,
    )
    .with_message("server at capacity, retry shortly".to_owned());
    projected_error_response(envelope, meta, cfg)
}

/// Builds the `PARTITION_OVERLOADED` response (Tier 5): this partition exceeded its concurrency
/// share while global capacity may remain — the caller (that partition) should back off, so it's
/// owned by the caller, retryable. Retryable ⇒ projects to `503` + `Retry-After` (never `429`).
fn partition_overloaded_response(meta: Meta, cfg: RespCfg) -> AxumResponse {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "PARTITION_OVERLOADED".to_owned(),
        true,
        ErrorOwner::Caller,
    )
    .with_message("partition concurrency limit reached, retry shortly".to_owned());
    projected_error_response(envelope, meta, cfg)
}

/// Outcome of acquiring the per-partition (Tier 5) + global bulkhead (Tier 1) permits.
enum Admission {
    /// Both granted — hold for the execution. `partition_permit` is `None` when no partition
    /// was supplied or fairness is disabled.
    Granted {
        /// Per-partition permit (Tier 5).
        partition_permit: Option<OwnedSemaphorePermit>,
        /// Global bulkhead permit (Tier 1).
        global: OwnedSemaphorePermit,
    },
    /// The partition exceeded its per-partition share (`429 PARTITION_OVERLOADED`).
    PartitionBusy,
    /// The global bulkhead is saturated (`429 OVERLOADED`).
    GlobalBusy,
}

/// Acquires the partition (Tier 5) + global bulkhead (Tier 1) permits, recording the shed
/// and returning the ready-to-send `429` response when either limit is hit. `Ok` carries
/// the permits to hold across the execution span. `busy_meta` is consumed only on a shed.
fn admit(
    state: &AppState,
    partition: Option<&str>,
    busy_meta: Meta,
) -> Result<(Option<OwnedSemaphorePermit>, OwnedSemaphorePermit), Box<AxumResponse>> {
    match acquire_permits(state, partition) {
        Admission::Granted {
            partition_permit,
            global,
        } => Ok((partition_permit, global)),
        Admission::PartitionBusy => {
            state.metrics.record_overload_partition();
            Err(Box::new(partition_overloaded_response(
                busy_meta,
                state.resp_cfg(),
            )))
        }
        Admission::GlobalBusy => {
            state.metrics.record_overload_global();
            Err(Box::new(overloaded_response(busy_meta, state.resp_cfg())))
        }
    }
}

/// Acquires the per-partition permit (if a partition is supplied and fairness is on) then the
/// global bulkhead permit. Per-partition first, so a noisy partition fast-fails on its own
/// share before consuming a global slot.
fn acquire_permits(state: &AppState, partition: Option<&str>) -> Admission {
    let partition_permit = if let (Some(limiter), Some(id)) = (&state.partition_limiter, partition)
    {
        let Some(permit) = limiter.try_acquire(id) else {
            return Admission::PartitionBusy;
        };
        Some(permit)
    } else {
        None
    };
    match Arc::clone(&state.limiter).try_acquire_owned() {
        Ok(global) => Admission::Granted {
            partition_permit,
            global,
        },
        Err(_too_busy) => Admission::GlobalBusy,
    }
}

/// Builds a zero-timing `Meta` for an early error return, cloning the correlation fields
/// (which the caller still needs on the continuing path).
fn base_error_meta(
    trace_id: &str,
    script_bytes: usize,
    context_bytes: usize,
    key: Option<&str>,
    partition: Option<&str>,
) -> Meta {
    Meta::new(trace_id.to_owned(), script_bytes, context_bytes, 0)
        .with_key(key.map(str::to_owned))
        .with_partition(partition.map(str::to_owned))
}

/// Reads the partition key from the `X-Partition-Key` header (trimmed, non-empty). Takes
/// precedence over the request body's `partition` field.
fn header_partition(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-partition-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Resolves the trusted identity in trusted-header mode and applies the hard-reject gates before
/// any body work: an anonymous caller, a suspended principal, or (for tenant-scoped work) a missing
/// tenant id is refused with a `403`. Returns `Ok(None)` in single-tenant mode (no trusted headers
/// consulted), `Ok(Some(identity))` when accepted, or `Err(response)` to reject.
fn resolve_identity(
    state: &AppState,
    headers: &HeaderMap,
    trace_id: &str,
) -> Result<Option<TrustedIdentity>, Box<AxumResponse>> {
    let Some(trusted) = state.trusted.as_ref() else {
        return Ok(None);
    };
    let identity = TrustedIdentity::from_headers(headers, &trusted.headers);
    if identity.anonymous {
        emit_denied(
            state,
            Some(&identity),
            trace_id,
            "ANONYMOUS_FORBIDDEN",
            None,
        );
        return Err(Box::new(identity_rejected(
            trace_id,
            "ANONYMOUS_FORBIDDEN",
            "anonymous callers may not execute code",
        )));
    }
    if identity.suspended {
        emit_denied(
            state,
            Some(&identity),
            trace_id,
            "SUSPENDED_FORBIDDEN",
            None,
        );
        return Err(Box::new(identity_rejected(
            trace_id,
            "SUSPENDED_FORBIDDEN",
            "a suspended principal may not execute code",
        )));
    }
    if identity.tenant.is_none() {
        emit_denied(state, Some(&identity), trace_id, "TENANT_REQUIRED", None);
        return Err(Box::new(identity_rejected(
            trace_id,
            "TENANT_REQUIRED",
            "trusted-header mode requires a tenant identity",
        )));
    }
    // Acting-org assurance (nexus N5, fail-closed): the edge must assert, per request, that the
    // tenant id is the caller's *authorized acting org* by setting the trusted scope header to
    // `acting`. Missing or any other value means the edge has not satisfied N5 for this request —
    // reject before any session or execution so a silent multi-org mis-scope becomes a loud 403.
    // `runlet` checks only the scope label; it never derives the org relationship itself (D3).
    if identity.scope.as_deref() != Some("acting") {
        emit_denied(
            state,
            Some(&identity),
            trace_id,
            "ACTING_SCOPE_REQUIRED",
            None,
        );
        return Err(Box::new(identity_rejected(
            trace_id,
            "ACTING_SCOPE_REQUIRED",
            "trusted-header mode requires the edge to assert acting-org scope",
        )));
    }
    // Audit trail: bind the trusted tenant + user id to this request's trace so a support query can
    // grep one id across the mesh (the user id is otherwise only carried for audit).
    tracing::debug!(
        trace_id,
        tenant = identity.tenant.as_deref(),
        user = identity.user.as_deref(),
        "trusted identity accepted"
    );
    Ok(Some(identity))
}

/// Builds the `403` authorization-failure response for a rejected trusted identity (anonymous /
/// suspended / tenant-less). A request-category envelope owned by the caller, never retryable.
fn identity_rejected(trace_id: &str, code: &str, message: &str) -> AxumResponse {
    let meta = Meta::new(trace_id.to_owned(), 0, 0, 0);
    system_error_response(request_error(code, message.to_owned()), 403, meta)
}

/// The fairness + cache key for a request. In trusted mode it is the trusted tenant id and the
/// caller-asserted source is ignored; otherwise it is the caller-asserted source (single-tenant).
fn resolve_partition(
    identity: Option<&TrustedIdentity>,
    caller_asserted: Option<String>,
) -> Option<String> {
    identity.map_or(caller_asserted, |id| id.tenant.clone())
}

/// The capabilities a request exercises — the flat logical resource names in `config.io` plus the
/// in-engine `http`/`s3` when their config is present. Used by the member-authz gate (which now
/// keys `capability_entitlements` by logical resource name, not kind).
fn requested_capabilities(config: &RequestConfig) -> Vec<&str> {
    let mut names: Vec<&str> = config.io.enabled_names();
    if !config.allowed_hosts.is_empty() {
        names.push("http");
    }
    if config.s3.is_some() {
        names.push("s3");
    }
    names
}

/// Coarse member-capability authz (trusted mode): reject a member lacking the entitlement a
/// requested capability requires. `None` = permitted (or not in trusted mode / no gate configured).
fn enforce_member_authz(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    config: &RequestConfig,
    trace_id: &str,
) -> Option<Box<AxumResponse>> {
    let (Some(trusted), Some(id)) = (state.trusted.as_ref(), identity) else {
        return None;
    };
    let requested = requested_capabilities(config);
    match authorize_capabilities(&trusted.capability_entitlements, &requested, id) {
        Ok(()) => None,
        Err(denied) => {
            let message = format!(
                "capability `{}` requires entitlement `{}`",
                denied.capability, denied.required
            );
            emit_denied(
                state,
                identity,
                trace_id,
                "ENTITLEMENT_REQUIRED",
                Some(json!({ "capability": denied.capability, "required": denied.required })),
            );
            let meta = Meta::new(trace_id.to_owned(), 0, 0, 0);
            Some(Box::new(system_error_response(
                request_error("ENTITLEMENT_REQUIRED", message),
                403,
                meta,
            )))
        }
    }
}

/// Per-tenant quota admission (trusted mode). On success returns the in-flight guard to hold across
/// the execution (or `None` when quota is disabled / not in trusted mode); on over-limit returns the
/// `429 QUOTA_EXCEEDED` response. `meta` is built lazily, only on the reject path.
fn enforce_quota<F: FnOnce() -> Meta>(
    state: &AppState,
    identity: Option<&TrustedIdentity>,
    trace_id: &str,
    meta: F,
) -> Result<Option<QuotaGuard>, Box<AxumResponse>> {
    let (Some(trusted), Some(id)) = (state.trusted.as_ref(), identity) else {
        return Ok(None);
    };
    let Some(quota) = trusted.quota.as_ref() else {
        return Ok(None);
    };
    let Some(tenant) = id.tenant.as_deref() else {
        return Ok(None);
    };
    match quota.admit(tenant, id.plan.as_deref()) {
        Ok(guard) => Ok(Some(guard)),
        Err(exceeded) => {
            emit_denied(
                state,
                identity,
                trace_id,
                "QUOTA_EXCEEDED",
                Some(json!({
                    "plan": exceeded.plan,
                    "limit": exceeded.limit,
                    "usage": exceeded.usage,
                })),
            );
            Err(Box::new(quota_exceeded_response(
                &exceeded,
                meta(),
                state.resp_cfg(),
            )))
        }
    }
}

/// Builds the `QUOTA_EXCEEDED` envelope carrying the plan, limit, and current usage — the structured
/// over-limit result. Retryable (a concurrency cap frees as executions finish), owned by the caller
/// (the tenant is over its plan). Shared by the single-`/execute` response and the per-item `/batch`
/// path.
fn quota_exceeded_envelope(exceeded: &QuotaExceeded) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "QUOTA_EXCEEDED".to_owned(),
        true,
        ErrorOwner::Caller,
    )
    .with_message(format!(
        "tenant quota exceeded for plan `{plan}`: {usage} in-flight at limit {limit}",
        plan = exceeded.plan,
        usage = exceeded.usage,
        limit = exceeded.limit,
    ))
}

/// Builds the `QUOTA_EXCEEDED` single-`/execute` response from the shared envelope + meta.
/// Retryable (a concurrency cap frees as executions finish) ⇒ projects to `503` + `Retry-After`,
/// **not `429`** — the header's *value* distinguishes a per-second rate-limit from a hard cap, the
/// status stays a truthful "retry" for a one-digit worker.
fn quota_exceeded_response(exceeded: &QuotaExceeded, meta: Meta, cfg: RespCfg) -> AxumResponse {
    projected_error_response(quota_exceeded_envelope(exceeded), meta, cfg)
}

/// Enforces the optional `/execute` bearer gate. Returns `Some(401)` when a token is
/// configured and the request doesn't present a matching one; `None` when auth passes or no
/// token is configured (auth handled upstream / loopback bind).
fn enforce_auth(state: &AppState, headers: &HeaderMap) -> Option<AxumResponse> {
    let expected = state.access_token.as_deref()?;
    if request_authorized(headers, expected) {
        return None;
    }
    state.metrics.record_rejection();
    Some(unauthorized_response())
}

/// Returns `true` if the request carries a valid `Authorization: Bearer <token>` matching
/// `expected`. The token is compared in constant time so a timing side-channel can't recover
/// it byte by byte.
fn request_authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers.get(AUTHORIZATION).and_then(|raw| raw.to_str().ok()) else {
        return false;
    };
    let Some(token) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        return false;
    };
    ct_eq(token.trim().as_bytes(), expected.as_bytes())
}

/// Builds the `401 UNAUTHORIZED` response for a missing/invalid bearer token.
fn unauthorized_response() -> AxumResponse {
    let trace_id = Uuid::new_v4().to_string();
    let envelope = request_error("UNAUTHORIZED", "missing or invalid bearer token".to_owned());
    system_error_response(envelope, 401, Meta::new(trace_id, 0, 0, 0))
}

/// Builds a `request`-category envelope (the caller's fault, never retryable).
fn request_error(code: &str, message: String) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCategory::Request,
        ErrorSource::Request,
        code.to_owned(),
        false,
        ErrorOwner::Caller,
    )
    .with_message(message)
}

/// Builds the success response, or a `MALFORMED_RESPONSE` error if the JS envelope
/// can't be parsed.
///
/// Secrets need no output scrubbing: their plaintext never enters JS — it stays
/// Rust-side as opaque handles (see `sys.rs`), so a script can only ever return the
/// `"[secret:NAME]"` placeholder, never the value. The `{data,error}` borrow stays
/// zero-copy.
fn success_response(
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
fn handler_envelope_response(
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
struct HandlerRetryable {
    /// The opt-in retry hint the box projects to the status line (`503`/`422`); absent ⇒ park.
    #[serde(default)]
    retryable: Option<bool>,
}

/// Projects a handler-returned `error` value onto the HTTP status line (D5). A JSON-null `error`
/// is a real success (`200`); any non-null `error` is never `2xx` — it parks at `422` unless the
/// handler opted into retry with a top-level `retryable: true` (then `503` + `Retry-After`). A
/// non-object or `retryable`-less error is treated as absent ⇒ `422`.
fn handler_status(error: &RawValue) -> Projected {
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
fn engine_error_response(
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
fn projected_error_response(error: ErrorEnvelope, meta: Meta, cfg: RespCfg) -> AxumResponse {
    projected_error_response_with_effects(error, meta, &[], None, cfg)
}

/// [`projected_error_response`] carrying any effects captured before an execution error
/// (capture-on-failure) and — when the trusted gateway requested capture — the diagnostic `logs`
/// mirror. Pre-execution errors have no effects/logs and go through the thin wrapper.
fn projected_error_response_with_effects(
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
/// the configured default — the box's circuit breakers live in `fabricd`, so there is no local
/// cool-down to read; the status already says "retry", the header only bounds the backoff.
fn add_retry_after(response: &mut AxumResponse, seconds: u32) {
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        let _prev = response.headers_mut().insert(RETRY_AFTER, value);
    }
}

/// Serializes a `{ data: null, error, meta }` response at the given status (no effects, no logs).
fn system_error_response(error: ErrorEnvelope, status: u16, meta: Meta) -> AxumResponse {
    system_error_response_with_effects(error, status, meta, &[], None)
}

/// Serializes a `{ data: null, error, meta, effects?, logs? }` response at the given status,
/// carrying any effects captured before the failure (omitted when empty) and the gateway-gated
/// diagnostic `logs` mirror (omitted unless capture was requested).
fn system_error_response_with_effects(
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

#[cfg(test)]
mod tests {
    //! `/execute` bearer-auth gate: `Authorization` header parsing (constant-time compare itself is
    //! tested in `runlet_wire::ct`).

    use super::request_authorized;
    use axum::http::HeaderMap;
    use axum::http::HeaderValue;
    use axum::http::header::AUTHORIZATION;

    /// A `HeaderMap` carrying a single `Authorization` header value.
    fn with_auth(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        drop(headers.insert(AUTHORIZATION, HeaderValue::from_static(value)));
        headers
    }

    /// A matching bearer token authorizes, case-insensitively on the scheme.
    #[test]
    fn authorized_accepts_matching_bearer() {
        assert!(
            request_authorized(&with_auth("Bearer s3cret"), "s3cret"),
            "exact match authorizes"
        );
        assert!(
            request_authorized(&with_auth("bearer s3cret"), "s3cret"),
            "lowercase scheme authorizes"
        );
    }

    /// A wrong, prefix-less, empty, or absent token is rejected.
    #[test]
    fn authorized_rejects_bad_or_missing() {
        assert!(
            !request_authorized(&with_auth("Bearer wrong"), "s3cret"),
            "wrong token rejected"
        );
        assert!(
            !request_authorized(&with_auth("s3cret"), "s3cret"),
            "missing Bearer prefix rejected"
        );
        assert!(
            !request_authorized(&HeaderMap::new(), "s3cret"),
            "absent header rejected"
        );
        assert!(
            !request_authorized(&with_auth("Bearer "), "s3cret"),
            "empty token rejected"
        );
    }
}

#[cfg(test)]
mod log_mirror_tests {
    //! §3 response mirror: `logs` is present iff the trusted gateway requested capture, on both the
    //! 2xx and non-2xx paths, and absent otherwise (byte-compatible with the prior contract). Also
    //! the caller-can't-force-capture policy (an untrusted request never captures).

    use super::{Effect, ErrorEnvelope, LogEntry, LogPolicy, Meta, Response, SystemErrorResponse};
    use crate::identity::{RunMode, TrustedIdentity};
    use runlet_core::engine::LogLevel;
    use runlet_core::errors::{ErrorCategory, ErrorOwner, ErrorSource};
    use serde_json::value::RawValue;

    /// One representative captured entry.
    fn entry() -> LogEntry {
        LogEntry {
            level: LogLevel::Info,
            template: "hi {n}".to_owned(),
            properties: RawValue::from_string("{\"n\":1}".to_owned())
                .unwrap_or_else(|_e| unreachable!()),
            message: "hi 1".to_owned(),
            seq: 0,
            offset_us: None,
        }
    }

    /// A minimal meta for serialization.
    fn meta() -> Meta {
        Meta::new("trace-1".to_owned(), 0, 0, 0)
    }

    /// A raw `null` value for the success envelope fields.
    fn raw_null() -> Box<RawValue> {
        RawValue::from_string("null".to_owned()).unwrap_or_else(|_e| unreachable!())
    }

    /// A success response with a captured mirror serializes a top-level `logs` array; without it the
    /// field is entirely absent (byte-compatible with `{data, error, meta}`).
    #[test]
    fn success_mirror_present_only_when_captured() {
        let logs = [entry()];
        let data = raw_null();
        let error = raw_null();
        let no_effects: [Effect; 0] = [];
        let with = Response {
            data: &data,
            error: &error,
            meta: meta(),
            effects: &no_effects,
            logs: Some(&logs),
        };
        let json = serde_json::to_string(&with).unwrap_or_default();
        assert!(
            json.contains("\"logs\""),
            "captured run carries logs: {json}"
        );
        assert!(json.contains("hi 1"), "the entry is serialized");

        let without = Response {
            data: &data,
            error: &error,
            meta: meta(),
            effects: &no_effects,
            logs: None,
        };
        let json = serde_json::to_string(&without).unwrap_or_default();
        assert!(
            !json.contains("\"logs\""),
            "no capture ⇒ no logs field: {json}"
        );
    }

    /// A non-2xx (system error) response carries the partial trail when captured (capture-on-failure)
    /// and omits the field otherwise.
    #[test]
    fn error_mirror_present_only_when_captured() {
        let logs = [entry()];
        let no_effects: [Effect; 0] = [];
        let script_error = || {
            ErrorEnvelope::new(
                ErrorCategory::Script,
                ErrorSource::Handler,
                "SCRIPT_ERROR".to_owned(),
                false,
                ErrorOwner::Developer,
            )
        };
        let with = SystemErrorResponse {
            data: None,
            error: script_error(),
            meta: meta(),
            effects: &no_effects,
            logs: Some(&logs),
        };
        let json = serde_json::to_string(&with).unwrap_or_default();
        assert!(
            json.contains("\"logs\""),
            "captured error carries the trail: {json}"
        );

        let without = SystemErrorResponse {
            data: None,
            error: script_error(),
            meta: meta(),
            effects: &no_effects,
            logs: None,
        };
        let json = serde_json::to_string(&without).unwrap_or_default();
        assert!(
            !json.contains("\"logs\""),
            "no capture ⇒ no logs field: {json}"
        );
    }

    /// A request with no trusted identity (the caller-asserted / single-tenant path) never captures
    /// and is always live — a caller cannot force the mirror or pick the mode.
    #[test]
    fn untrusted_request_never_captures() {
        let policy = LogPolicy::resolve(None);
        assert!(!policy.capture, "no gateway ⇒ no capture");
        assert_eq!(policy.mode, RunMode::Live, "no gateway ⇒ live");
    }

    /// The trusted capture flag drives the mirror; the mode is carried through for stream routing.
    #[test]
    fn trusted_capture_flag_drives_policy() {
        let id = TrustedIdentity {
            capture: true,
            mode: RunMode::Test,
            ..TrustedIdentity::default()
        };
        let policy = LogPolicy::resolve(Some(&id));
        assert!(policy.capture, "trusted capture requests the mirror");
        assert_eq!(
            policy.mode,
            RunMode::Test,
            "test mode is response-mirror-only"
        );
    }
}

#[cfg(test)]
mod request_io_tests {
    //! The box-side `config.io` interpretation (byo-capabilities: a flat allowlist of logical
    //! names): which names are enabled, which need the broker vs box-direct, and the session-open
    //! `WireInit` (the flat `resources` list). Name→kind/endpoint resolution itself lives in
    //! `fabricd`; box-direct bindings live in the operator's global config.

    use super::{RequestIo, wire_init};
    use std::collections::HashMap;

    /// A `RequestIo` naming a flat list of logical resources.
    fn io(names: &[&str]) -> RequestIo {
        RequestIo(names.iter().map(|name| (*name).to_owned()).collect())
    }

    /// `any()` is true iff a name is listed; `enabled_names()` is the flat allowlist.
    #[test]
    fn any_and_enabled_names_track_the_flat_allowlist() {
        let listed = io(&["orders", "cache"]);
        assert!(listed.any(), "a named resource means the io port is wired");
        assert_eq!(
            listed.enabled_names(),
            vec!["orders", "cache"],
            "the flat allowlist is the enabled set"
        );

        let empty = RequestIo::default();
        assert!(!empty.any(), "no names → no io port");
        assert!(empty.enabled_names().is_empty(), "no names enabled");
    }

    /// `broker_names` excludes names bound box-direct in the global local map; a box-direct-only
    /// request needs no broker session.
    #[test]
    fn broker_names_exclude_box_direct_bindings() {
        let mut local = HashMap::new();
        drop(local.insert("pricing".to_owned(), "http://127.0.0.1:8080".to_owned()));
        let listed = io(&["orders", "pricing"]);
        assert_eq!(
            listed.broker_names(&local),
            vec!["orders".to_owned()],
            "only the non-local name goes to the broker"
        );

        let local_only = io(&["pricing"]);
        assert!(
            local_only.broker_names(&local).is_empty(),
            "a box-direct-only request opens no broker session"
        );
    }

    /// `wire_init` carries the flat resource list, the deadline, and the trusted tenant.
    #[test]
    fn wire_init_carries_flat_resources() {
        let init = wire_init(
            vec!["orders".to_owned(), "cache".to_owned()],
            std::time::Duration::from_millis(1500),
            Some("ws_acme"),
        );
        assert_eq!(
            init.resources,
            vec!["orders".to_owned(), "cache".to_owned()]
        );
        assert_eq!(init.timeout_ms, 1500);
        assert_eq!(
            init.tenant.as_deref(),
            Some("ws_acme"),
            "trusted tenant carried on the handshake"
        );
    }
}

#[cfg(test)]
mod partition_tests {
    //! The fairness/cache key source: caller-asserted in single-tenant mode, the trusted tenant id
    //! (ignoring any caller-asserted value) in trusted mode.

    use super::resolve_partition;
    use crate::identity::TrustedIdentity;

    /// Without a trusted identity the caller-asserted value is used (single-tenant behavior).
    #[test]
    fn single_tenant_uses_caller_asserted() {
        let key = resolve_partition(None, Some("caller-key".to_owned()));
        assert_eq!(key.as_deref(), Some("caller-key"));
    }

    /// In trusted mode the key is the trusted tenant id and the caller-asserted value is ignored.
    #[test]
    fn trusted_uses_tenant_and_ignores_caller() {
        let identity = TrustedIdentity {
            tenant: Some("ws_acme".to_owned()),
            ..TrustedIdentity::default()
        };
        let key = resolve_partition(Some(&identity), Some("spoofed-partition".to_owned()));
        assert_eq!(
            key.as_deref(),
            Some("ws_acme"),
            "trusted tenant wins; caller-asserted partition is ignored"
        );
    }
}

#[cfg(test)]
mod trusted_pipeline_tests {
    //! End-to-end `/execute` in trusted-header mode (driving the handler directly): anonymous /
    //! suspended / tenant-less / entitlement rejections (which return before any execution) and a
    //! quota over-limit, plus a permitted deterministic execution. Egress is not wired (no sidecar),
    //! so tests exercise deterministic scripts / pre-execution gates — tenant-scoped egress
    //! resolution is covered in `fabric_backends::resources`.

    use super::{
        AppState, ExecRequest, RequestConfig, RequestIo, TrustedRuntime, default_context, execute,
    };
    use crate::config::TrustedHeaders;
    use crate::events::{Event, Sink};
    use crate::quota::{PlanLimit, TenantQuota};
    use crate::sidecar::SidecarTransport;
    use axum::Json;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse as _;
    use runlet_core::config::EngineConfig;
    use runlet_core::host::{HostSettings, LogicHost};
    use runlet_core::metrics::Metrics;
    use runlet_core::modules::ModuleRegistry;
    use runlet_core::pool::JsPool;
    use runlet_core::registry::ScriptRegistry;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::Semaphore;

    /// Builds an app state in trusted mode with the given member-authz gate + optional quota, a tiny
    /// runtime pool, and no egress sidecar.
    fn state(gate: HashMap<String, String>, quota: Option<TenantQuota>) -> AppState {
        let mut engine = EngineConfig::default();
        engine
            .resolve_limits()
            .unwrap_or_else(|_err| unreachable!("engine limits resolve"));
        let pool = JsPool::new(engine, Arc::new(ModuleRegistry::default()))
            .unwrap_or_else(|_err| unreachable!("pool init"));
        let registry = Arc::new(ScriptRegistry::default());
        let host = LogicHost::new(
            pool,
            Arc::clone(&registry),
            HostSettings {
                limits: engine,
                allow_private_targets: false,
            },
        );
        AppState {
            host,
            registry,
            engine_cfg: engine,
            error_debug: false,
            limiter: Arc::new(Semaphore::new(8)),
            partition_limiter: None,
            transport: SidecarTransport::None,
            local_resources: Arc::new(HashMap::new()),
            local_client: reqwest::Client::new(),
            metrics: Arc::new(Metrics::default()),
            bulkhead_capacity: 8,
            access_token: None,
            trusted: Some(Arc::new(TrustedRuntime {
                headers: TrustedHeaders::default(),
                capability_entitlements: gate,
                quota,
            })),
            events: None,
            event_dropped: None,
            log_event_dropped: None,
            batch: crate::config::BatchConfig::default(),
            timeout_retryable: true,
            retry_after_seconds: 1,
            default_currency: None,
        }
    }

    /// A `HeaderMap` from `(name, value)` pairs.
    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            drop(map.insert(*name, HeaderValue::from_static(value)));
        }
        map
    }

    /// Drives `/execute` with the given headers + request config and returns the HTTP status.
    async fn run(state: &AppState, hdrs: HeaderMap, config: RequestConfig) -> StatusCode {
        let req = ExecRequest {
            script: Some("function handler(ctx){ return { data: 1 }; }".to_owned()),
            key: None,
            partition: None,
            context: default_context(),
            config,
        };
        execute(State(state.clone()), hdrs, Ok(Json(req)))
            .await
            .into_response()
            .status()
    }

    /// A test sink that captures each event's serialized JSON, so a flow test can assert the
    /// per-path emit-count invariant without exposing `Event`'s private fields.
    #[derive(Debug, Default)]
    struct CapturingSink {
        /// Serialized JSON of every recorded event.
        lines: Mutex<Vec<String>>,
    }

    impl Sink for CapturingSink {
        fn record(&self, event: Event) {
            let Ok(json) = serde_json::to_string(&event) else {
                return;
            };
            if let Ok(mut lines) = self.lines.lock() {
                lines.push(json);
            }
        }

        fn record_log(&self, event: Event) {
            // Diagnostic log events land in the same capture list (uniform for assertions).
            self.record(event);
        }
    }

    impl CapturingSink {
        /// A snapshot of the captured event JSON lines.
        fn lines(&self) -> Vec<String> {
            self.lines
                .lock()
                .map_or_else(|_| Vec::new(), |guard| guard.clone())
        }
    }

    /// Executed request ⇒ exactly one `usage` + one `allowed` audit event.
    #[tokio::test]
    async fn events_executed_emits_usage_plus_allowed_audit() {
        let sink = Arc::new(CapturingSink::default());
        let sink_concrete = Arc::clone(&sink);
        let sink_dyn: Arc<dyn Sink> = sink_concrete;
        let mut app = state(HashMap::new(), None);
        app.events = Some(sink_dyn);
        let hdrs = headers(&[("x-workspace-id", "ws_a"), ("x-tenant-scope", "acting")]);
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::OK
        );
        let lines = sink.lines();
        let usage = lines
            .iter()
            .filter(|line| line.contains("\"type\":\"usage\""))
            .count();
        let allowed = lines
            .iter()
            .filter(|line| {
                line.contains("\"type\":\"audit\"") && line.contains("\"decision\":\"allowed\"")
            })
            .count();
        assert_eq!(usage, 1, "one usage event for an executed request");
        assert_eq!(allowed, 1, "one allowed audit event");
        assert!(
            lines
                .iter()
                .all(|line| line.contains("\"tenant\":\"ws_a\"")),
            "events attributed to the tenant"
        );
    }

    /// Rejected request ⇒ zero `usage`, exactly one `denied` audit carrying the reason.
    #[tokio::test]
    async fn events_denied_emits_audit_only_with_reason() {
        let sink = Arc::new(CapturingSink::default());
        let sink_concrete = Arc::clone(&sink);
        let sink_dyn: Arc<dyn Sink> = sink_concrete;
        let mut plans = HashMap::new();
        let _ = plans.insert("denied".to_owned(), PlanLimit { max_concurrent: 0 });
        let mut app = state(HashMap::new(), Some(TenantQuota::new(plans)));
        app.events = Some(sink_dyn);
        let hdrs = headers(&[
            ("x-workspace-id", "ws_a"),
            ("x-tenant-scope", "acting"),
            ("x-tenant-plan", "denied"),
        ]);
        // Over-quota is retryable ⇒ `503` (never `429`, whose `4xx` digit would make a status-line
        // worker park a response that only needs to wait).
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let lines = sink.lines();
        let usage = lines
            .iter()
            .filter(|line| line.contains("\"type\":\"usage\""))
            .count();
        let denied = lines
            .iter()
            .filter(|line| {
                line.contains("\"decision\":\"denied\"") && line.contains("QUOTA_EXCEEDED")
            })
            .count();
        assert_eq!(usage, 0, "no usage event for a rejected request");
        assert_eq!(denied, 1, "one denied audit carrying the quota reason");
    }

    /// An anonymous caller is rejected `403` before any execution.
    #[tokio::test]
    async fn anonymous_is_forbidden() {
        let app = state(HashMap::new(), None);
        let hdrs = headers(&[("x-workspace-id", "ws_a"), ("x-auth-anonymous", "true")]);
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::FORBIDDEN
        );
    }

    /// A suspended principal is rejected `403`.
    #[tokio::test]
    async fn suspended_is_forbidden() {
        let app = state(HashMap::new(), None);
        let hdrs = headers(&[("x-workspace-id", "ws_a"), ("x-user-suspended", "true")]);
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::FORBIDDEN
        );
    }

    /// A request with no tenant header is rejected `403` in trusted mode.
    #[tokio::test]
    async fn missing_tenant_is_forbidden() {
        let app = state(HashMap::new(), None);
        assert_eq!(
            run(&app, HeaderMap::new(), RequestConfig::default()).await,
            StatusCode::FORBIDDEN
        );
    }

    /// A member lacking the entitlement a requested capability needs is rejected `403` (before the
    /// capability — and before the missing sidecar — is reached).
    #[tokio::test]
    async fn member_without_entitlement_is_forbidden() {
        let mut gate = HashMap::new();
        drop(gate.insert("db".to_owned(), "db.write".to_owned()));
        let app = state(gate, None);
        let hdrs = headers(&[
            ("x-workspace-id", "ws_a"),
            ("x-tenant-scope", "acting"),
            ("x-user-entitlements", "mail.send"),
        ]);
        let config = RequestConfig {
            io: RequestIo(vec!["db".to_owned()]),
            ..RequestConfig::default()
        };
        assert_eq!(run(&app, hdrs, config).await, StatusCode::FORBIDDEN);
    }

    /// A tenant over its plan's hard cap (a `max_concurrent: 0` plan denies the first request) gets
    /// `429 QUOTA_EXCEEDED`.
    #[tokio::test]
    async fn over_quota_is_rejected() {
        let mut plans = HashMap::new();
        let _ = plans.insert("denied".to_owned(), PlanLimit { max_concurrent: 0 });
        let app = state(HashMap::new(), Some(TenantQuota::new(plans)));
        let hdrs = headers(&[
            ("x-workspace-id", "ws_a"),
            ("x-tenant-scope", "acting"),
            ("x-tenant-plan", "denied"),
        ]);
        // Retryable capacity fault ⇒ `503`, not `429` (see the projection invariant).
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// A well-formed trusted request with the acting-org assurance executes (`200`).
    #[tokio::test]
    async fn permitted_request_executes() {
        let app = state(HashMap::new(), None);
        let hdrs = headers(&[
            ("x-workspace-id", "ws_a"),
            ("x-tenant-scope", "acting"),
            ("x-user-id", "u1"),
        ]);
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::OK
        );
    }

    /// The acting-org gate matrix (nexus N5): a tenant-scoped request missing the scope assurance,
    /// or carrying a non-`acting` value, is rejected `403` before any execution; `acting` proceeds.
    #[tokio::test]
    async fn acting_scope_gate_matrix() {
        let app = state(HashMap::new(), None);

        // Absent scope → rejected.
        let missing = headers(&[("x-workspace-id", "ws_a")]);
        assert_eq!(
            run(&app, missing, RequestConfig::default()).await,
            StatusCode::FORBIDDEN,
            "a tenant-scoped request without the acting-org assurance is refused"
        );

        // Non-`acting` scope → rejected.
        let wrong = headers(&[("x-workspace-id", "ws_a"), ("x-tenant-scope", "home")]);
        assert_eq!(
            run(&app, wrong, RequestConfig::default()).await,
            StatusCode::FORBIDDEN,
            "a non-acting scope is refused"
        );

        // `acting` scope → proceeds (executes the deterministic script).
        let ok = headers(&[("x-workspace-id", "ws_a"), ("x-tenant-scope", "acting")]);
        assert_eq!(
            run(&app, ok, RequestConfig::default()).await,
            StatusCode::OK,
            "the authorized acting-org request proceeds"
        );
    }

    /// The scope header is consulted only in trusted mode: with trusted mode off, a request carrying
    /// no scope header (and no trusted headers at all) executes normally — the gate never runs.
    #[tokio::test]
    async fn non_trusted_mode_ignores_scope() {
        let mut app = state(HashMap::new(), None);
        app.trusted = None;
        assert_eq!(
            run(&app, HeaderMap::new(), RequestConfig::default()).await,
            StatusCode::OK,
            "non-trusted mode does not consult the scope header"
        );
    }
}

#[cfg(test)]
mod batch_tests {
    //! `POST /batch` driven directly through the handler: order preservation + `id` echo, per-item
    //! isolation, partial failure, batch-level caps (empty / too-many / xor-key item), the D6
    //! response-size truncation, and — the D5 GraphQL-batch-attack guard — per-item authorization.
    //! Egress is not wired (no sidecar), so items run deterministic scripts.

    use super::{
        AppState, BatchItem, BatchRequest, RequestConfig, RequestIo, TrustedRuntime, batch,
    };
    use crate::config::{BatchConfig, TrustedHeaders};
    use crate::quota::{PlanLimit, TenantQuota};
    use crate::sidecar::SidecarTransport;
    use axum::Json;
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse as _;
    use runlet_core::config::EngineConfig;
    use runlet_core::host::{HostSettings, LogicHost};
    use runlet_core::metrics::Metrics;
    use runlet_core::modules::ModuleRegistry;
    use runlet_core::pool::JsPool;
    use runlet_core::registry::ScriptRegistry;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    /// Builds an app state with a small warm pool, no sidecar, and the given trusted runtime.
    fn state(trusted: Option<Arc<TrustedRuntime>>) -> AppState {
        let mut engine = EngineConfig::default();
        engine
            .resolve_limits()
            .unwrap_or_else(|_err| unreachable!("engine limits resolve"));
        let pool = JsPool::new(engine, Arc::new(ModuleRegistry::default()))
            .unwrap_or_else(|_err| unreachable!("pool init"));
        let registry = Arc::new(ScriptRegistry::default());
        let host = LogicHost::new(
            pool,
            Arc::clone(&registry),
            HostSettings {
                limits: engine,
                allow_private_targets: false,
            },
        );
        AppState {
            host,
            registry,
            engine_cfg: engine,
            error_debug: false,
            limiter: Arc::new(Semaphore::new(8)),
            partition_limiter: None,
            transport: SidecarTransport::None,
            local_resources: Arc::new(HashMap::new()),
            local_client: reqwest::Client::new(),
            metrics: Arc::new(Metrics::default()),
            bulkhead_capacity: 8,
            access_token: None,
            trusted,
            events: None,
            event_dropped: None,
            log_event_dropped: None,
            batch: BatchConfig::default(),
            timeout_retryable: true,
            retry_after_seconds: 1,
            default_currency: None,
        }
    }

    /// A trusted runtime with the given capability→entitlement gate and no quota.
    fn trusted_runtime(gate: HashMap<String, String>) -> Arc<TrustedRuntime> {
        Arc::new(TrustedRuntime {
            headers: TrustedHeaders::default(),
            capability_entitlements: gate,
            quota: None,
        })
    }

    /// A batch item running the given inline script (no id, default config).
    fn item(script: &str) -> BatchItem {
        BatchItem {
            script: Some(script.to_owned()),
            key: None,
            context: super::default_context(),
            config: RequestConfig::default(),
            id: None,
        }
    }

    /// A `HeaderMap` from `(name, value)` pairs.
    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            drop(map.insert(*name, HeaderValue::from_static(value)));
        }
        map
    }

    /// Drives `POST /batch` (items only, no lifecycle) and returns `(status, parsed-body)`.
    async fn run(state: &AppState, hdrs: HeaderMap, items: Vec<BatchItem>) -> (StatusCode, Value) {
        run_full(state, hdrs, items, None, None, None).await
    }

    /// Drives `POST /batch` with the full `before`/`shared`/`after` lifecycle and returns
    /// `(status, parsed-body)`.
    async fn run_full(
        state: &AppState,
        hdrs: HeaderMap,
        items: Vec<BatchItem>,
        before: Option<BatchItem>,
        shared: Option<&str>,
        after: Option<BatchItem>,
    ) -> (StatusCode, Value) {
        let shared = shared.map(|json| {
            super::RawValue::from_string(json.to_owned())
                .unwrap_or_else(|_err| super::default_context())
        });
        let request = BatchRequest {
            items,
            before,
            shared,
            after,
        };
        let response = batch(State(state.clone()), hdrs, Ok(Json(request)))
            .await
            .into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    /// Results preserve request order regardless of completion order, and a client `id` is echoed.
    #[tokio::test]
    async fn preserves_order_and_echoes_id() {
        let app = state(None);
        let items = vec![
            BatchItem {
                id: Some("a".to_owned()),
                ..item("function handler(){ return { data: 1 }; }")
            },
            BatchItem {
                id: Some("b".to_owned()),
                ..item("function handler(){ return { data: 2 }; }")
            },
            BatchItem {
                id: Some("c".to_owned()),
                ..item("function handler(){ return { data: 3 }; }")
            },
        ];
        let (status, body) = run(&app, HeaderMap::new(), items).await;
        assert_eq!(status, StatusCode::OK, "an admitted batch returns 200");
        let results = body["results"].as_array().expect("results array");
        assert_eq!(results.len(), 3);
        for (index, data, id) in [(0, 1, "a"), (1, 2, "b"), (2, 3, "c")] {
            assert_eq!(results[index]["data"], data, "positional data");
            assert_eq!(results[index]["id"], id, "echoed id");
        }
        assert_eq!(body["meta"]["items"], 3);
        assert_eq!(body["meta"]["ok"], 3);
        assert_eq!(body["meta"]["failed"], 0);
    }

    /// One item's failure is isolated: it carries an error envelope, the others still succeed, and the
    /// batch reports `ok`/`failed` accordingly — still HTTP 200 (design D4).
    #[tokio::test]
    async fn partial_failure_is_isolated() {
        let app = state(None);
        let items = vec![
            item("function handler(){ return { data: 1 }; }"),
            item("function handler(){ throw new Error('boom'); }"),
            item("function handler(){ return { data: 3 }; }"),
        ];
        let (status, body) = run(&app, HeaderMap::new(), items).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "partial failure is still a 200 batch"
        );
        let results = body["results"].as_array().expect("results array");
        assert_eq!(results[0]["data"], 1);
        assert!(
            !results[1]["error"].is_null(),
            "the failing item carries an error"
        );
        assert_eq!(results[2]["data"], 3);
        assert_eq!(body["meta"]["ok"], 2);
        assert_eq!(body["meta"]["failed"], 1);
    }

    /// Each item runs in a fresh global scope — a mutation by one item is invisible to another.
    #[tokio::test]
    async fn items_are_isolated_from_each_other() {
        let app = state(None);
        let items = vec![
            item("globalThis.leak = 42; function handler(){ return { data: 'set' }; }"),
            item("function handler(){ return { data: typeof globalThis.leak }; }"),
        ];
        let (_status, body) = run(&app, HeaderMap::new(), items).await;
        let results = body["results"].as_array().expect("results array");
        assert_eq!(
            results[1]["data"], "undefined",
            "a global set by one item does not leak into another"
        );
    }

    /// An empty batch is rejected whole with a request-category 400 before any item runs.
    #[tokio::test]
    async fn empty_batch_is_rejected() {
        let app = state(None);
        let (status, body) = run(&app, HeaderMap::new(), Vec::new()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "EMPTY_BATCH");
    }

    /// A batch over `max_items` is rejected whole with a 400 and no item executes.
    #[tokio::test]
    async fn too_many_items_is_rejected() {
        let mut app = state(None);
        app.batch.max_items = 2;
        let items = vec![
            item("function handler(){ return { data: 1 }; }"),
            item("function handler(){ return { data: 2 }; }"),
            item("function handler(){ return { data: 3 }; }"),
        ];
        let (status, body) = run(&app, HeaderMap::new(), items).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "BATCH_TOO_LARGE");
    }

    /// A malformed item (both `script` and `key`) fails only itself; the rest of the batch runs.
    #[tokio::test]
    async fn malformed_item_fails_only_itself() {
        let app = state(None);
        let items = vec![
            BatchItem {
                key: Some("also-a-key".to_owned()),
                ..item("function handler(){ return { data: 1 }; }")
            },
            item("function handler(){ return { data: 2 }; }"),
        ];
        let (status, body) = run(&app, HeaderMap::new(), items).await;
        assert_eq!(status, StatusCode::OK);
        let results = body["results"].as_array().expect("results array");
        assert_eq!(results[0]["error"]["code"], "SCRIPT_XOR_KEY");
        assert_eq!(results[1]["data"], 2, "the well-formed item still runs");
    }

    /// The D6 response-size cap truncates the offending item to a size-limit envelope rather than
    /// buffering an unbounded response — the earlier item still returns its data.
    #[tokio::test]
    async fn response_size_cap_truncates_offending_item() {
        let mut app = state(None);
        // A small first item fits; the second returns a large blob that clearly blows the cap.
        app.batch.max_response_bytes = 2000;
        let items = vec![
            item("function handler(){ return { data: 1 }; }"),
            item("function handler(){ return { data: 'x'.repeat(5000) }; }"),
        ];
        let (status, body) = run(&app, HeaderMap::new(), items).await;
        assert_eq!(status, StatusCode::OK);
        let results = body["results"].as_array().expect("results array");
        assert_eq!(results[0]["data"], 1, "the first item fits and returns");
        assert_eq!(
            results[1]["error"]["code"], "BATCH_RESPONSE_TRUNCATED",
            "the item that would blow the cap is truncated"
        );
    }

    /// D5 (GraphQL-batch-attack guard): per-item authorization is evaluated for EVERY item — an item
    /// requesting a gated capability the member lacks fails, while a plain item in the same batch
    /// still runs. A batch cannot smuggle an operation past the per-request authz gate.
    #[tokio::test]
    async fn authorization_is_per_item() {
        let mut gate = HashMap::new();
        drop(gate.insert("db".to_owned(), "db.write".to_owned()));
        let app = state(Some(trusted_runtime(gate)));
        let hdrs = headers(&[
            ("x-workspace-id", "ws_a"),
            ("x-tenant-scope", "acting"),
            ("x-user-entitlements", "mail.send"),
        ]);
        let items = vec![
            BatchItem {
                config: RequestConfig {
                    io: RequestIo(vec!["db".to_owned()]),
                    ..RequestConfig::default()
                },
                ..item("function handler(){ return { data: 'db' }; }")
            },
            item("function handler(){ return { data: 'plain' }; }"),
        ];
        let (status, body) = run(&app, hdrs, items).await;
        assert_eq!(status, StatusCode::OK, "the batch itself is admitted");
        let results = body["results"].as_array().expect("results array");
        assert_eq!(
            results[0]["error"]["code"], "ENTITLEMENT_REQUIRED",
            "the item requesting a gated capability is denied per item"
        );
        assert_eq!(
            results[1]["data"], "plain",
            "an ungated item in the same batch still runs"
        );
    }

    // ===== batch-lifecycle-phases: before → items → after (RQ1–RQ3) =====

    /// A trusted runtime carrying a per-tenant quota (and no capability gate).
    fn trusted_runtime_with_quota(quota: TenantQuota) -> Arc<TrustedRuntime> {
        Arc::new(TrustedRuntime {
            headers: TrustedHeaders::default(),
            capability_entitlements: HashMap::new(),
            quota: Some(quota),
        })
    }

    /// A `before`/`after` phase invocation running the given inline script (no id, default config).
    fn phase(script: &str) -> BatchItem {
        item(script)
    }

    /// 4.1 — Backward compat: a batch with only `items` (no lifecycle) yields no `summary`/
    /// `summary_error`, byte-for-byte the pre-change shape.
    #[tokio::test]
    async fn no_lifecycle_omits_summary_fields() {
        let app = state(None);
        let items = vec![item("function handler(){ return { data: 1 }; }")];
        let (status, body) = run(&app, HeaderMap::new(), items).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("summary").is_none(), "no summary without after");
        assert!(
            body.get("summary_error").is_none(),
            "no summary_error without after"
        );
        assert_eq!(body["results"][0]["data"], 1);
    }

    /// 4.2 — Phase ordering by data dependency: items observe `before`'s output (so `before`
    /// completed first) and `after` reduces every item's result (so all items completed first).
    #[tokio::test]
    async fn phases_run_before_items_after() {
        let app = state(None);
        let items = vec![
            item("function handler(ctx){ return { data: ctx.shared.n }; }"),
            item("function handler(ctx){ return { data: ctx.shared.n }; }"),
            item("function handler(ctx){ return { data: ctx.shared.n }; }"),
        ];
        let before = Some(phase("function handler(){ return { data: { n: 10 } }; }"));
        let after = Some(phase(
            "function handler(ctx){ let s = 0; for (const r of ctx.results) { s += r.data; } return { data: s }; }",
        ));
        let (status, body) = run_full(&app, HeaderMap::new(), items, before, None, after).await;
        assert_eq!(status, StatusCode::OK);
        for index in 0..3 {
            assert_eq!(body["results"][index]["data"], 10, "item read before's output");
        }
        assert_eq!(body["summary"], 30, "after reduced all three item results");
    }

    /// 4.3 — Shared context: items see `before`'s output merged over the `shared` seed (with
    /// `before` winning on key collision), and a sibling item's mutation is invisible to another
    /// (each item parses its own immutable copy).
    #[tokio::test]
    async fn shared_context_merges_and_is_isolated() {
        let app = state(None);
        let items = vec![
            // Reads the merged view (seed `from_seed`/`k`, before `from_before`/`k`; before wins `k`).
            item(
                "function handler(ctx){ return { data: { seed: ctx.shared.from_seed, before: ctx.shared.from_before, k: ctx.shared.k } }; }",
            ),
            // Attempts to mutate its own copy of the shared context.
            item("function handler(ctx){ ctx.shared.k = 999; return { data: 'mutated' }; }"),
            // A later item must still see the original immutable value, not the sibling's write.
            item("function handler(ctx){ return { data: ctx.shared.k }; }"),
        ];
        let before = Some(phase(
            "function handler(){ return { data: { from_before: 1, k: 2 } }; }",
        ));
        let seed = r#"{"from_seed":9,"k":1}"#;
        let (status, body) =
            run_full(&app, HeaderMap::new(), items, before, Some(seed), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["results"][0]["data"]["seed"], 9, "seed value visible");
        assert_eq!(body["results"][0]["data"]["before"], 1, "before value visible");
        assert_eq!(body["results"][0]["data"]["k"], 2, "before wins the collision");
        assert_eq!(
            body["results"][2]["data"], 2,
            "a sibling's mutation does not leak into another item"
        );
    }

    /// 4.4 — Fetch-collapse: a single `before` produces the shared value that every one of N items
    /// reads, so a per-item fetch is unnecessary (the fetch runs once, in `before`). The literal
    /// egress-count assertion is integration-level; here the structural once-produced/N-read property
    /// is proven by the identical value across items.
    #[tokio::test]
    async fn before_output_is_shared_by_all_items() {
        let app = state(None);
        let items = (0..4)
            .map(|_idx| item("function handler(ctx){ return { data: ctx.shared.token }; }"))
            .collect();
        let before = Some(phase(
            "function handler(){ return { data: { token: 'fetched-once' } }; }",
        ));
        let (status, body) = run_full(&app, HeaderMap::new(), items, before, None, None).await;
        assert_eq!(status, StatusCode::OK);
        let results = body["results"].as_array().expect("results array");
        assert_eq!(results.len(), 4);
        for result in results {
            assert_eq!(
                result["data"], "fetched-once",
                "every item reads the one shared value"
            );
        }
    }

    /// 4.5 — `before` barrier: a throwing `before` aborts the whole batch non-200 with no `results`
    /// array (no item ran) and no `after`.
    #[tokio::test]
    async fn before_throw_is_a_barrier() {
        let app = state(None);
        let items = vec![item("function handler(){ return { data: 1 }; }")];
        let before = Some(phase("function handler(){ throw new Error('boom'); }"));
        let after = Some(phase("function handler(){ return { data: 'never' }; }"));
        let (status, body) = run_full(&app, HeaderMap::new(), items, before, None, after).await;
        assert_ne!(status, StatusCode::OK, "a before failure is non-200");
        assert!(
            body.get("results").is_none(),
            "no item runs when before is the barrier"
        );
        assert!(body.get("summary").is_none(), "after never runs");
        assert!(!body["error"].is_null(), "the barrier carries the before error");
    }

    /// 4.6 — `after` reduce: a returning `after` surfaces its value as the top-level `summary`; a
    /// throwing `after` keeps HTTP 200 with `results` intact and reports `summary_error`.
    #[tokio::test]
    async fn after_summary_and_failure() {
        let app = state(None);
        let mk_items = || {
            vec![
                item("function handler(){ return { data: 1 }; }"),
                item("function handler(){ return { data: 2 }; }"),
            ]
        };
        // Success: reduce to the item count.
        let after_ok = Some(phase(
            "function handler(ctx){ return { data: { count: ctx.results.length } }; }",
        ));
        let (status, body) = run_full(&app, HeaderMap::new(), mk_items(), None, None, after_ok).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["summary"]["count"], 2, "after's value is the summary");
        assert!(body.get("summary_error").is_none(), "no error on success");

        // Failure: a throwing after keeps the 200 + results, reports summary_error.
        let after_err = Some(phase("function handler(){ throw new Error('reduce failed'); }"));
        let (status, body) =
            run_full(&app, HeaderMap::new(), mk_items(), None, None, after_err).await;
        assert_eq!(status, StatusCode::OK, "a failed after does not fail the batch");
        assert_eq!(body["results"][0]["data"], 1, "results stay intact");
        assert_eq!(body["results"][1]["data"], 2);
        assert!(body.get("summary").is_none(), "no summary on failure");
        assert!(
            !body["summary_error"].is_null(),
            "the after failure is surfaced as summary_error"
        );
    }

    /// 4.7a — Gates: `before` is subject to the same per-invocation quota an item is — a `0`-capacity
    /// plan denies it, aborting the batch as a barrier before any item runs.
    #[tokio::test]
    async fn before_is_quota_gated() {
        let mut plans = HashMap::new();
        let _ = plans.insert("denied".to_owned(), PlanLimit { max_concurrent: 0 });
        let app = state(Some(trusted_runtime_with_quota(TenantQuota::new(plans))));
        let hdrs = headers(&[
            ("x-workspace-id", "ws_a"),
            ("x-tenant-scope", "acting"),
            ("x-tenant-plan", "denied"),
        ]);
        let items = vec![item("function handler(){ return { data: 1 }; }")];
        let before = Some(phase("function handler(){ return { data: 1 }; }"));
        let (status, body) = run_full(&app, hdrs, items, before, None, None).await;
        assert_ne!(status, StatusCode::OK, "quota-denied before aborts the batch");
        assert_eq!(
            body["error"]["code"], "QUOTA_EXCEEDED",
            "before debits quota like an item"
        );
        assert!(body.get("results").is_none(), "no item runs past the barrier");
    }

    /// 4.7b — Gates: I/O in `before` is gated exactly as for an item — with no sidecar, a `before`
    /// naming a broker-resolved `io` resource fails closed (`EGRESS_UNAVAILABLE`), a barrier that runs
    /// no items. (The box HTTP front always runs the full profile; fail-closed egress is the box-level
    /// analogue of the spec's "profile denies I/O in lifecycle phases".)
    #[tokio::test]
    async fn before_io_fails_closed() {
        let app = state(None);
        let items = vec![item("function handler(){ return { data: 1 }; }")];
        let before = Some(BatchItem {
            config: RequestConfig {
                io: RequestIo(vec!["orders".to_owned()]),
                ..RequestConfig::default()
            },
            ..phase("function handler(ctx){ return { data: ctx.io ? 1 : 0 }; }")
        });
        let (status, body) = run_full(&app, HeaderMap::new(), items, before, None, None).await;
        assert_ne!(status, StatusCode::OK, "no sidecar ⇒ before I/O fails closed");
        assert_eq!(body["error"]["code"], "EGRESS_UNAVAILABLE");
        assert!(body.get("results").is_none(), "the barrier runs no items");
    }
}

#[cfg(test)]
mod execute_status_tests {
    //! `/execute` HTTP status = projection of the outcome (design D1/D5): a null-error success is
    //! `200`; a handler-returned error is never `2xx` — it parks at `422` unless the handler opts
    //! into retry (`retryable: true ⇒ 503` + `Retry-After`); the body is passed through verbatim.
    //! Also covers the engine-error projections reachable without a wired sidecar (oversize `413`,
    //! syntax `422`).

    use super::{AppState, ExecRequest, RequestConfig, default_context, execute};
    use crate::sidecar::SidecarTransport;
    use axum::Json;
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::header::RETRY_AFTER;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse as _, Response as AxumResponse};
    use runlet_core::config::EngineConfig;
    use runlet_core::host::{HostSettings, LogicHost};
    use runlet_core::metrics::Metrics;
    use runlet_core::modules::ModuleRegistry;
    use runlet_core::pool::JsPool;
    use runlet_core::registry::ScriptRegistry;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    /// A non-trusted (single-tenant, no sidecar) app state with a small warm pool.
    fn state() -> AppState {
        let mut engine = EngineConfig::default();
        engine
            .resolve_limits()
            .unwrap_or_else(|_err| unreachable!("engine limits resolve"));
        let pool = JsPool::new(engine, Arc::new(ModuleRegistry::default()))
            .unwrap_or_else(|_err| unreachable!("pool init"));
        let registry = Arc::new(ScriptRegistry::default());
        let host = LogicHost::new(
            pool,
            Arc::clone(&registry),
            HostSettings {
                limits: engine,
                allow_private_targets: false,
            },
        );
        AppState {
            host,
            registry,
            engine_cfg: engine,
            error_debug: false,
            limiter: Arc::new(Semaphore::new(8)),
            partition_limiter: None,
            transport: SidecarTransport::None,
            local_resources: Arc::new(HashMap::new()),
            local_client: reqwest::Client::new(),
            metrics: Arc::new(Metrics::default()),
            bulkhead_capacity: 8,
            access_token: None,
            trusted: None,
            events: None,
            event_dropped: None,
            log_event_dropped: None,
            batch: crate::config::BatchConfig::default(),
            timeout_retryable: true,
            retry_after_seconds: 7,
            default_currency: None,
        }
    }

    /// Drives `/execute` with an inline script and returns the raw response.
    async fn run(app: &AppState, script: &str) -> AxumResponse {
        let req = ExecRequest {
            script: Some(script.to_owned()),
            key: None,
            partition: None,
            context: default_context(),
            config: RequestConfig::default(),
        };
        execute(State(app.clone()), HeaderMap::new(), Ok(Json(req)))
            .await
            .into_response()
    }

    /// Splits a response into `(status, retry_after_header, parsed_body)`.
    async fn parts(response: AxumResponse) -> (StatusCode, Option<String>, Value) {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        (status, retry_after, body)
    }

    /// A handler returning only `data` (null error) is a real success: `200`, no `Retry-After`.
    #[tokio::test]
    async fn null_error_is_200() {
        let app = state();
        let (status, retry_after, body) =
            parts(run(&app, "function handler(){ return { data: 42 }; }").await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(retry_after.is_none(), "no Retry-After on a success");
        assert_eq!(body["data"], 42);
        assert!(body["error"].is_null());
    }

    /// A handler that opts into retry (`retryable: true`) parks the status at `503` with a
    /// `Retry-After` header, and the `error` body is passed through verbatim.
    #[tokio::test]
    async fn handler_opts_into_retry_503() {
        let app = state();
        let script =
            "function handler(){ return json(null, { message: 'later', retryable: true }); }";
        let (status, retry_after, body) = parts(run(&app, script).await).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            retry_after.as_deref(),
            Some("7"),
            "Retry-After seeded from the configured default"
        );
        assert_eq!(
            body["error"]["message"], "later",
            "the error body is verbatim"
        );
        assert_eq!(body["error"]["retryable"], true);
    }

    /// A handler that opts into park (`retryable: false`) is `422`, body verbatim, no `Retry-After`.
    #[tokio::test]
    async fn handler_opts_into_park_422() {
        let app = state();
        let script =
            "function handler(){ return json(null, { message: 'nope', retryable: false }); }";
        let (status, retry_after, body) = parts(run(&app, script).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(retry_after.is_none());
        assert_eq!(body["error"]["message"], "nope");
    }

    /// An un-annotated handler error (no `retryable` key) defaults to `422` (park), never `200`.
    #[tokio::test]
    async fn unannotated_handler_error_parks_422() {
        let app = state();
        let script = "function handler(){ return json(null, { message: 'name required' }); }";
        let (status, _retry_after, body) = parts(run(&app, script).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["error"]["message"], "name required",
            "body passed through unchanged"
        );
    }

    /// An uncaught handler throw is a non-retryable developer error ⇒ `422` (not the old `200`).
    #[tokio::test]
    async fn uncaught_throw_parks_422() {
        let app = state();
        let (status, retry_after, _body) =
            parts(run(&app, "function handler(){ throw new Error('boom'); }").await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(retry_after.is_none());
    }

    /// A syntax error parks at `422`.
    #[tokio::test]
    async fn syntax_error_parks_422() {
        let app = state();
        let (status, _retry_after, body) = parts(run(&app, "function handler( {").await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "SYNTAX_ERROR");
    }

    /// An oversized script is a caller fault that parks at `413`, not `400`.
    #[tokio::test]
    async fn oversize_script_is_413() {
        let mut app = state();
        app.engine_cfg.max_script_size = 32;
        let big = format!(
            "function handler(){{ return {{ data: '{}' }}; }}",
            "x".repeat(200)
        );
        let (status, _retry_after, body) = parts(run(&app, &big).await).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "SCRIPT_TOO_LARGE");
    }

    /// A successful run surfaces its `emit(kind, value)` effects as a top-level ordered list.
    #[tokio::test]
    async fn success_surfaces_effects() {
        let app = state();
        let script = "function handler(){ emit('decided', { tier: 3 }); \
            emit('note', 'ok'); return { data: 1 }; }";
        let (status, _retry_after, body) = parts(run(&app, script).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"], 1);
        assert_eq!(body["effects"][0]["kind"], "decided");
        assert_eq!(body["effects"][0]["value"]["tier"], 3);
        assert_eq!(body["effects"][1]["kind"], "note");
        assert_eq!(body["effects"][1]["value"], "ok");
    }

    /// A handler that emits then throws still surfaces the partial effects trail on the non-2xx
    /// response (capture-on-failure).
    #[tokio::test]
    async fn failing_run_surfaces_partial_effects() {
        let app = state();
        let script = "function handler(){ emit('finding', 7); throw new Error('boom'); }";
        let (status, _retry_after, body) = parts(run(&app, script).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["effects"][0]["kind"], "finding");
        assert_eq!(body["effects"][0]["value"], 7);
    }

    /// A run that never emits carries no `effects` key — byte-compatible with the prior
    /// `{data, error, meta}` envelope.
    #[tokio::test]
    async fn no_emit_omits_effects_key() {
        let app = state();
        let (_status, _retry_after, body) =
            parts(run(&app, "function handler(){ return { data: 1 }; }").await).await;
        assert!(
            body.get("effects").is_none(),
            "no effects key when nothing was emitted"
        );
    }
}

#[cfg(test)]
mod fail_closed_envelope_tests {
    //! The no-sidecar session error projects to the retryable `EGRESS_UNAVAILABLE` operator fault —
    //! the response half of the fail-closed egress invariant (the decision half lives in `sidecar`).

    use super::session_error_envelope;
    use crate::sidecar::SessionError;
    use runlet_core::errors::ErrorOwner;

    /// An absent/unreachable sidecar is a retryable operator fault carrying the `EGRESS_UNAVAILABLE`
    /// code — the box refuses egress rather than degrading to an ambient path.
    #[test]
    fn unavailable_maps_to_retryable_egress_unavailable() {
        let envelope = session_error_envelope(SessionError::Unavailable("no sidecar".to_owned()));
        assert_eq!(envelope.code(), "EGRESS_UNAVAILABLE");
        assert!(envelope.is_retryable());
        assert!(matches!(envelope.owner(), ErrorOwner::Operator));
    }
}
