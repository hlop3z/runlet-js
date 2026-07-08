//! HTTP handler for the `/execute` endpoint.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, StatusCode};
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
use runlet_core::engine::{EngineError, ExecOutcome};
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
use crate::events::{AuditBody, CapabilityOps, Event, EventBody, Sink, UsageBody};
use crate::identity::TrustedIdentity;
use crate::quota::{QuotaExceeded, QuotaGuard, TenantQuota};
use crate::sidecar::{SessionConn, SessionError, SidecarEgress, SidecarTransport, connect_session};

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
    /// `POST /batch` fan-out caps (item count + combined-input / total-response byte bounds). Copied
    /// from server config; the single-`/execute` path never reads it.
    pub(crate) batch: BatchConfig,
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
    /// Logical resources this invocation may reach, keyed by capability kind (e.g.
    /// `{"db":["orders-db"]}`). The names are sent to `fabricd`, which resolves them against its
    /// operator config; the request never carries endpoints or credentials.
    #[serde(default)]
    pub(crate) io: RequestIo,
}

/// The `config.io` allowlist: which logical resources the script may address, per capability
/// kind. The box selects the first named resource of each kind (single binding per kind) and
/// sends those names to `fabricd` in the session `WireInit`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RequestIo {
    /// `db` logical resource names.
    #[serde(default)]
    pub(crate) db: Vec<String>,
    /// `mongo` logical resource names.
    #[serde(default)]
    pub(crate) mongo: Vec<String>,
    /// `mail` logical resource names.
    #[serde(default)]
    pub(crate) mail: Vec<String>,
    /// `redis` logical resource names.
    #[serde(default)]
    pub(crate) redis: Vec<String>,
    /// `amq` logical resource names.
    #[serde(default)]
    pub(crate) amq: Vec<String>,
    /// `auth` logical resource names.
    #[serde(default)]
    pub(crate) auth: Vec<String>,
}

impl RequestIo {
    /// `true` if any driver capability is requested (so a `fabricd` session is needed).
    fn any(&self) -> bool {
        [
            &self.db,
            &self.mongo,
            &self.mail,
            &self.redis,
            &self.amq,
            &self.auth,
        ]
        .iter()
        .any(|names| !names.is_empty())
    }

    /// The registered capability names to enable for this request — a kind is enabled iff the
    /// request named a resource for it. Passed to the engine as `CapabilitySet.io`; a registered
    /// def's wrapper is injected only for a name in this list.
    fn enabled_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if !self.db.is_empty() {
            names.push("db");
        }
        if !self.mongo.is_empty() {
            names.push("mongo");
        }
        if !self.mail.is_empty() {
            names.push("mail");
        }
        if !self.redis.is_empty() {
            names.push("redis");
        }
        if !self.amq.is_empty() {
            names.push("amq");
        }
        if !self.auth.is_empty() {
            names.push("auth");
        }
        names
    }

    /// The `fabricd` session-open message: the first named resource per kind, the per-execution
    /// deadline, and the request's trusted tenant id (so `fabricd` scopes resolution to that
    /// tenant's bindings). `fabricd` resolves each name against its operator config.
    fn wire_init(&self, timeout: Duration, tenant: Option<&str>) -> WireInit {
        WireInit {
            db: self.db.first().cloned(),
            mongo: self.mongo.first().cloned(),
            mail: self.mail.first().cloned(),
            redis: self.redis.first().cloned(),
            amq: self.amq.first().cloned(),
            auth: self.auth.first().cloned(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            // The trusted tenant id, sourced only from the trusted-header extractor (never the
            // script). `None` on the single-tenant/loopback path.
            tenant: tenant.map(str::to_owned),
            // The token (QUIC path) is attached by `connect_session` from the transport's auth
            // provider — the box-request layer never sees it.
            token: None,
        }
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
    /// engine outcome, the driver-backed capabilities from the egress adapter. Only capabilities
    /// that actually ran get an entry.
    fn with_metrics(mut self, metrics: ExecMetrics, backend: BackendMetrics) -> Self {
        insert_io(&mut self.io, "http", metrics.http);
        insert_io(&mut self.io, "s3", metrics.s3);
        insert_io(&mut self.io, "db", backend.db);
        insert_io(&mut self.io, "mongo", backend.mongo);
        insert_io(&mut self.io, "mail", backend.mail);
        insert_io(&mut self.io, "redis", backend.redis);
        insert_io(&mut self.io, "amq", backend.amq);
        insert_io(&mut self.io, "auth", backend.auth);
        self
    }
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

/// Success response: JS-produced `{data, error}` as borrowed `RawValue` + Rust meta.
#[derive(Debug, Serialize)]
struct Response<'a> {
    /// The data field from the JS handler (borrowed, never copied).
    data: &'a RawValue,
    /// The error field from the JS handler (borrowed, never copied; D1 passthrough).
    error: &'a RawValue,
    /// Metadata computed by Rust.
    meta: Meta,
}

/// System-error response: `data` is `null`, `error` is the structured envelope.
#[derive(Debug, Serialize)]
struct SystemErrorResponse {
    /// Always `null` on a system error.
    data: Option<()>,
    /// The structured error envelope.
    error: ErrorEnvelope,
    /// Metadata computed by Rust.
    meta: Meta,
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

/// Maps a [`SessionError`] (opening the `fabricd` session) to a `(status, envelope)` pair: a
/// resolution failure is the caller's `400`, an unreachable/absent sidecar is a retryable `503`, a
/// protocol slip is a `500`. Shared by the single-`/execute` response path and the per-item `/batch`
/// path (which renders the envelope inside a `200` batch instead of setting the HTTP status).
fn session_error_envelope(err: SessionError) -> (u16, ErrorEnvelope) {
    match err {
        SessionError::Resolve { code, message } => (400, request_error(&code, message)),
        SessionError::Unavailable(message) => (
            503,
            ErrorEnvelope::new(
                ErrorCategory::Runtime,
                ErrorSource::Engine,
                "EGRESS_UNAVAILABLE".to_owned(),
                true,
                ErrorOwner::Operator,
            )
            .with_message(message),
        ),
        SessionError::Protocol(_raw) => (
            500,
            ErrorEnvelope::new(
                ErrorCategory::Runtime,
                ErrorSource::Engine,
                "EGRESS_PROTOCOL".to_owned(),
                false,
                ErrorOwner::Operator,
            )
            .with_message("egress protocol error".to_owned()),
        ),
    }
}

/// Maps a [`SessionError`] to the single-`/execute` HTTP response (status + envelope + meta).
fn session_error_response(err: SessionError, meta: Meta) -> AxumResponse {
    let (status, envelope) = session_error_envelope(err);
    system_error_response(envelope, status, meta)
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
    let error_debug = state.error_debug;

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
            let (status, envelope) = *rejection;
            let meta = Meta::new(trace_id, 0, context_bytes, 0)
                .with_key(key)
                .with_partition(partition);
            return system_error_response(envelope, status, meta);
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
        return system_error_response(request_error(code, message), 400, meta);
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

    // Open the `fabricd` egress session when any driver capability is requested. The box holds no
    // credentials: it sends the selected logical names + the trusted tenant id; `fabricd` resolves
    // them within that tenant's binding set. An unknown/out-of-tenant name (400), or an
    // unreachable/absent sidecar (503), is rejected here — before admission.
    let session = if config.io.any() {
        let init = config.io.wire_init(engine_cfg.timeout(), tenant.as_deref());
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
                return session_error_response(err, meta);
            }
        }
    } else {
        None
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
    let result = execute_blocking(ExecuteBlocking {
        host: state.host.clone(),
        handle: Handle::current(),
        timeout: engine_cfg.timeout(),
        session,
        source,
        context_json,
        config,
        cache_ns,
    })
    .await;

    // Execution finished — free the bulkhead + per-partition permits for the next request.
    drop(permit);
    drop(partition_permit);

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = Meta::new(trace_id, script_bytes, context_bytes, exec_time_us)
        .with_key(key)
        .with_partition(partition);
    build_response(result, base_meta, error_debug, &state, identity.as_ref())
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
    /// The pre-connected `fabricd` session, when the request named driver resources.
    session: Option<SessionConn>,
    /// The resolved script source (inline or registered).
    source: ScriptSource,
    /// Raw context JSON handed straight to `QuickJS`.
    context_json: String,
    /// Per-request capability configuration.
    config: RequestConfig,
    /// Bytecode-cache namespace (the fairness key) — keeps byte-identical source from different
    /// tenants from sharing a cache entry.
    cache_ns: Option<String>,
}

/// Runs one invocation to completion on a blocking thread — the shared execute core for `/execute`
/// and each `/batch` item. Wraps the pre-connected `fabricd` session as the egress, runs the
/// invocation under the full-capability profile, then drains the session's driver metrics (the
/// round-trips + drain `block_on` must run on the `spawn_blocking` thread, never a runtime worker).
async fn execute_blocking(
    params: ExecuteBlocking,
) -> Result<(Result<Outcome, EngineError>, BackendMetrics), task::JoinError> {
    let ExecuteBlocking {
        host,
        handle,
        timeout,
        session,
        source,
        context_json,
        config,
        cache_ns,
    } = params;
    task::spawn_blocking(move || -> (Result<Outcome, EngineError>, BackendMetrics) {
        let adapter =
            session.map(|conn| Arc::new(SidecarEgress::new(conn, handle.clone(), timeout)));
        let egress: Option<Arc<dyn Egress>> = adapter.as_ref().map(|metered| {
            // Upcast `Arc<SidecarEgress>` → `Arc<dyn Egress>`; the turbofish pins the source type so
            // the clone resolves before the coercion (the original `adapter` stays for draining).
            let dynamic: Arc<dyn Egress> = Arc::<SidecarEgress>::clone(metered);
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
        if let Some(port) = egress {
            invocation = invocation.egress(port);
        }
        if let Some(namespace) = cache_ns.as_deref() {
            invocation = invocation.cache_namespace(namespace);
        }
        let outcome = host.run(invocation);
        let metrics =
            adapter.map_or_else(BackendMetrics::default, |metered| metered.drain_metrics());
        (outcome, metrics)
    })
    .await
}

// ===== POST /batch — independent per-item fan-out over the single-execute machinery =====

/// A `/batch` request body: an ordered list of independent items. No atomicity, no cross-item
/// ordering guarantee during execution — only the results array preserves request order.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchRequest {
    /// The items to execute (validated for count/size before any admission).
    #[serde(default)]
    items: Vec<BatchItem>,
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
#[derive(Debug, Serialize)]
struct BatchResponse {
    /// One rendered `{data, error, meta, id?}` envelope per item, in request order.
    results: Vec<Box<RawValue>>,
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

/// The `/batch` request pipeline: batch-level gates (auth → identity → parse → caps), then a bounded
/// concurrent fan-out over the items, then order-preserving assembly with the D6 response-size cap.
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
    let items = req.items;

    // Batch-level caps (D3), before any item is admitted or executed.
    if let Err(rejected) = validate_batch(&state, &items, identity.as_ref(), &trace_id) {
        state.metrics.record_rejection();
        return *rejected;
    }

    // Shared fairness/partition key (request-level): the trusted tenant in trusted mode, else the
    // caller-asserted header. Every item keys off this — a batch cannot split its fairness bucket.
    let partition = resolve_partition(identity.as_ref(), header_partition(&headers));

    let start = Instant::now();
    let count = items.len();
    // Bound this batch's concurrency to its fair share so it cannot monopolize the pool (D2): the
    // per-partition ceiling when fairness is on, else the global bulkhead capacity. Items beyond the
    // ceiling queue on this gate rather than fast-failing (unlike single `/execute`).
    let gate = Arc::new(Semaphore::new(batch_ceiling(&state)));
    let mut set: JoinSet<(usize, RenderedItem)> = JoinSet::new();
    for (index, item) in items.into_iter().enumerate() {
        let task_state = state.clone();
        let task_gate = Arc::clone(&gate);
        let task_identity = identity.clone();
        let task_partition = partition.clone();
        let task_trace = trace_id.clone();
        let _abort = set.spawn(async move {
            let rendered = run_batch_item(BatchItemCtx {
                state: &task_state,
                gate: task_gate,
                identity: task_identity.as_ref(),
                partition: task_partition.as_deref(),
                trace_id: &task_trace,
                item,
            })
            .await;
            (index, rendered)
        });
    }

    // Collect into positional slots so the results array preserves request order regardless of
    // completion order. A slot left empty (a task that somehow panicked) is filled defensively.
    let mut slots: Vec<Option<RenderedItem>> = (0..count).map(|_idx| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((index, rendered)) = joined
            && let Some(slot) = slots.get_mut(index)
        {
            *slot = Some(rendered);
        }
    }

    let duration_ms = start.elapsed().as_millis();
    assemble_batch(slots, trace_id, duration_ms, state.batch.max_response_bytes)
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

    // Open the fabricd session when the item names driver resources.
    let session = if config.io.any() {
        let tenant = identity.and_then(|trusted| trusted.tenant.as_deref());
        let init = config.io.wire_init(state.engine_cfg.timeout(), tenant);
        match connect_session(&state.transport, &init).await {
            Ok(conn) => Some(conn),
            Err(err) => {
                emit_denied(state, identity, trace_id, "EGRESS_UNAVAILABLE", None);
                let (_status, envelope) = session_error_envelope(err);
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
    } else {
        None
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
        source,
        context_json: context.get().to_owned(),
        config,
        cache_ns: partition.map(str::to_owned),
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
    result: Result<(Result<Outcome, EngineError>, BackendMetrics), task::JoinError>,
    base_meta: Meta,
    id: Option<&str>,
) -> RenderedItem {
    let metrics: &Metrics = &state.metrics;
    metrics.observe_execution(base_meta.exec_time_us);
    match result {
        Ok((Ok(exec), backend)) => {
            record_capability_latencies(metrics, &exec.metrics, &backend);
            let meta = base_meta.with_metrics(exec.metrics, backend);
            match exec.result {
                ExecOutcome::Success(js_json) => {
                    emit_executed(state, identity, &meta, "success");
                    metrics.record_success();
                    render_success_item(&js_json, &meta, id, state.error_debug)
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome);
                    metrics.record_engine_error(&engine_err);
                    render_engine_error_item(engine_err, &meta, id, state.error_debug)
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            render_engine_error_item(engine_err, &base_meta, id, state.error_debug)
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            render_engine_error_item(engine_err, &base_meta, id, state.error_debug)
        }
    }
}

/// Serializes a success item from the JS `{data, error}` output + meta + id. A JS output that does
/// not parse is rendered as a `MALFORMED_RESPONSE` error item instead (mirrors [`success_response`]).
fn render_success_item(
    js_json: &str,
    meta: &Meta,
    id: Option<&str>,
    error_debug: bool,
) -> RenderedItem {
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
                    .into_envelope(error_debug);
            render_error_item(&envelope, meta, id)
        }
    }
}

/// Serializes an engine-error item, logging the raw cause server-side keyed by `trace_id` (mirrors
/// [`engine_error_response`]).
fn render_engine_error_item(
    err: EngineError,
    meta: &Meta,
    id: Option<&str>,
    error_debug: bool,
) -> RenderedItem {
    warn!(trace_id = %meta.trace_id, error = ?err, "batch item system error");
    let envelope = err.into_envelope(error_debug);
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

/// Assembles the final `/batch` response in request order, enforcing the total-response-bytes cap
/// (D6): an item whose bytes would push the running total past the cap is truncated to a classified
/// size-limit error envelope rather than buffered. Counts ok/failed for the batch summary.
fn assemble_batch(
    slots: Vec<Option<RenderedItem>>,
    trace_id: String,
    duration_ms: u128,
    max_response_bytes: usize,
) -> AxumResponse {
    let count = slots.len();
    let mut results: Vec<Box<RawValue>> = Vec::with_capacity(count);
    let mut ok = 0_usize;
    let mut failed = 0_usize;
    let mut used = 0_usize;
    for slot in slots {
        let rendered = slot.unwrap_or_else(|| internal_error_item(&trace_id));
        let projected = used.saturating_add(rendered.bytes());
        let item = if projected > max_response_bytes {
            let truncated = truncated_item(&trace_id);
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
    let meta = BatchMeta {
        items: count,
        ok,
        failed,
        duration_ms,
        trace_id,
    };
    (StatusCode::OK, Json(BatchResponse { results, meta })).into_response()
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

/// Turns the `spawn_blocking` result into the final HTTP response, attaching metrics to
/// `meta` on success and classifying the error otherwise. Also emits the per-request `usage` +
/// `allowed` audit events (the single executed-request event site, Change C).
fn build_response(
    result: Result<(Result<Outcome, EngineError>, BackendMetrics), task::JoinError>,
    base_meta: Meta,
    error_debug: bool,
    state: &AppState,
    identity: Option<&TrustedIdentity>,
) -> AxumResponse {
    let metrics: &Metrics = &state.metrics;
    // Record latency for every execution that ran (shed/rejected requests return earlier).
    metrics.observe_execution(base_meta.exec_time_us);
    match result {
        Ok((Ok(exec), backend)) => {
            record_capability_latencies(metrics, &exec.metrics, &backend);
            // `exec.effects` (the declarative `emit` buffer) is intentionally ignored by the
            // HTTP front — it is for non-HTTP consumers of `runlet-core`.
            let meta = base_meta.with_metrics(exec.metrics, backend);
            match exec.result {
                ExecOutcome::Success(js_json) => {
                    emit_executed(state, identity, &meta, "success");
                    metrics.record_success();
                    record_span_outcome("success");
                    success_response(&js_json, meta, error_debug)
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome);
                    metrics.record_engine_error(&engine_err);
                    record_span_outcome(outcome);
                    engine_error_response(engine_err, meta, error_debug)
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            record_span_outcome(outcome);
            engine_error_response(engine_err, base_meta, error_debug)
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome);
            metrics.record_engine_error(&engine_err);
            record_span_outcome(outcome);
            engine_error_response(engine_err, base_meta, error_debug)
        }
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

/// Builds the `429 OVERLOADED` response when the bulkhead is saturated: a runtime-category
/// envelope, retryable, owned by the operator (capacity, not the caller's request).
fn overloaded_response(meta: Meta) -> AxumResponse {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "OVERLOADED".to_owned(),
        true,
        ErrorOwner::Operator,
    )
    .with_message("server at capacity, retry shortly".to_owned());
    system_error_response(envelope, 429, meta)
}

/// Builds the `429 PARTITION_OVERLOADED` response (Tier 5): this partition exceeded its
/// concurrency share while global capacity may remain — the caller (that partition) should
/// back off, so it's owned by the caller, retryable.
fn partition_overloaded_response(meta: Meta) -> AxumResponse {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        "PARTITION_OVERLOADED".to_owned(),
        true,
        ErrorOwner::Caller,
    )
    .with_message("partition concurrency limit reached, retry shortly".to_owned());
    system_error_response(envelope, 429, meta)
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
            Err(Box::new(partition_overloaded_response(busy_meta)))
        }
        Admission::GlobalBusy => {
            state.metrics.record_overload_global();
            Err(Box::new(overloaded_response(busy_meta)))
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

/// The capability kinds a request exercises — the driver kinds named in `config.io` plus the
/// in-engine `http`/`s3` when their config is present. Used by the member-authz gate.
fn requested_capabilities(config: &RequestConfig) -> Vec<&'static str> {
    let io = &config.io;
    let mut kinds = Vec::new();
    if !io.db.is_empty() {
        kinds.push("db");
    }
    if !io.mongo.is_empty() {
        kinds.push("mongo");
    }
    if !io.mail.is_empty() {
        kinds.push("mail");
    }
    if !io.redis.is_empty() {
        kinds.push("redis");
    }
    if !io.amq.is_empty() {
        kinds.push("amq");
    }
    if !io.auth.is_empty() {
        kinds.push("auth");
    }
    if !config.allowed_hosts.is_empty() {
        kinds.push("http");
    }
    if config.s3.is_some() {
        kinds.push("s3");
    }
    kinds
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
            Err(Box::new(quota_exceeded_response(&exceeded, meta())))
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

/// Builds the `429 QUOTA_EXCEEDED` single-`/execute` response from the shared envelope + meta.
fn quota_exceeded_response(exceeded: &QuotaExceeded, meta: Meta) -> AxumResponse {
    system_error_response(quota_exceeded_envelope(exceeded), 429, meta)
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
fn success_response(js_json: &str, meta: Meta, error_debug: bool) -> AxumResponse {
    match serde_json::from_str::<Envelope<'_>>(js_json) {
        Ok(env) => (
            StatusCode::OK,
            Json(Response {
                data: env.data,
                error: env.error,
                meta,
            }),
        )
            .into_response(),
        Err(parse_err) => engine_error_response(
            EngineError::Malformed(format!("malformed handler response: {parse_err}")),
            meta,
            error_debug,
        ),
    }
}

/// Maps a classified [`EngineError`] to its envelope (debug-gated) + HTTP status, and
/// logs the full (raw) error server-side keyed by `trace_id` — so the raw cause is
/// always captured for support even when `error_debug` strips it from the response.
fn engine_error_response(err: EngineError, meta: Meta, error_debug: bool) -> AxumResponse {
    let status = err.http_status();
    warn!(trace_id = %meta.trace_id, status, error = ?err, "execute system error");
    let envelope = err.into_envelope(error_debug);
    system_error_response(envelope, status, meta)
}

/// Serializes a `{ data: null, error, meta }` response at the given status.
fn system_error_response(error: ErrorEnvelope, status: u16, meta: Meta) -> AxumResponse {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        code,
        Json(SystemErrorResponse {
            data: None,
            error,
            meta,
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
mod request_io_tests {
    //! The box-side `config.io` interpretation: which capabilities are requested, the engine
    //! gates, and the `fabricd` session-open `WireInit` (the first name per kind). Name→config
    //! resolution itself now lives in `fabricd` (`fabric_backends::resolve`).

    use super::RequestIo;

    /// A `RequestIo` naming one db resource (other kinds empty).
    fn io_db(name: &str) -> RequestIo {
        RequestIo {
            db: vec![name.to_owned()],
            ..RequestIo::default()
        }
    }

    /// `any()` is true iff some kind is named; `enabled_names()` mirrors per-kind presence.
    #[test]
    fn any_and_enabled_names_track_named_kinds() {
        let io = io_db("orders-db");
        assert!(io.any(), "a named db means a session is needed");
        assert_eq!(
            io.enabled_names(),
            vec!["db"],
            "only the named kind is enabled"
        );

        let empty = RequestIo::default();
        assert!(!empty.any(), "no names → no session");
        assert!(empty.enabled_names().is_empty(), "no names enabled");
    }

    /// `wire_init` carries the first name per kind and the deadline.
    #[test]
    fn wire_init_selects_first_name() {
        let io = RequestIo {
            db: vec!["orders-db".to_owned(), "ignored".to_owned()],
            ..RequestIo::default()
        };
        let init = io.wire_init(std::time::Duration::from_millis(1500), Some("ws_acme"));
        assert_eq!(init.db.as_deref(), Some("orders-db"), "first name selected");
        assert_eq!(init.mongo, None, "unnamed kinds stay None");
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
            batch: crate::config::BatchConfig::default(),
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
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::TOO_MANY_REQUESTS
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
            io: RequestIo {
                db: vec!["orders-db".to_owned()],
                ..RequestIo::default()
            },
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
        assert_eq!(
            run(&app, hdrs, RequestConfig::default()).await,
            StatusCode::TOO_MANY_REQUESTS
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
            metrics: Arc::new(Metrics::default()),
            bulkhead_capacity: 8,
            access_token: None,
            trusted,
            events: None,
            event_dropped: None,
            batch: BatchConfig::default(),
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

    /// Drives `POST /batch` and returns `(status, parsed-body)`.
    async fn run(state: &AppState, hdrs: HeaderMap, items: Vec<BatchItem>) -> (StatusCode, Value) {
        let response = batch(State(state.clone()), hdrs, Ok(Json(BatchRequest { items })))
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
                    io: RequestIo {
                        db: vec!["orders-db".to_owned()],
                        ..RequestIo::default()
                    },
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
}
