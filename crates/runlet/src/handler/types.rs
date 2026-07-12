//! Request/response data types and shared application state for the HTTP handler.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::sync::Semaphore;

use runlet_core::config::EngineConfig;
use runlet_core::host::LogicHost;
use runlet_core::metrics::Metrics;
use runlet_core::partition::PartitionLimiter;
use runlet_core::registry::ScriptRegistry;
use runlet_core::s3::S3Config;
use runlet_core::sys::SysConfig;
use runlet_wire::wire::WireInit;

use crate::broker::BrokerTransport;
use crate::config::{BatchConfig, TrustedHeaders};
use crate::events::Sink;
use crate::quota::TenantQuota;

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
    /// How the box reaches the egress broker (local UDS, remote QUIC, or none). The box
    /// links no driver and holds no credentials: a request that names a driver resource in
    /// `config.io` opens a session over this transport to the broker, which resolves the name against
    /// its own operator config and performs the I/O. [`BrokerTransport::None`] ⇒ driver
    /// capabilities are unavailable (`503 EGRESS_UNAVAILABLE`).
    pub(crate) transport: BrokerTransport,
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
    pub(crate) const fn resp_cfg(&self) -> RespCfg {
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
pub(crate) struct RespCfg {
    /// Include `error.debug` (stack + raw cause) in the envelope.
    pub(crate) error_debug: bool,
    /// `TIMEOUT` retryability knob (governs only the `TIMEOUT` fault's `retryable`).
    pub(crate) timeout_retryable: bool,
    /// `Retry-After` seconds attached to a retryable `503`/`500`.
    pub(crate) retry_after_seconds: u32,
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
pub(crate) static DEFAULT_CONTEXT: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("{}".into()).unwrap_or_else(|_err| unreachable!()));

/// Pre-allocated `Box<RawValue>` for `null` — used as default envelope field.
pub(crate) static RAW_NULL: LazyLock<Box<RawValue>> =
    LazyLock::new(|| RawValue::from_string("null".into()).unwrap_or_else(|_err| unreachable!()));

/// Request body for script execution.
#[derive(Debug, Deserialize)]
pub(crate) struct ExecRequest {
    /// Inline JavaScript source to evaluate (exactly one of `script` / `key`).
    pub(crate) script: Option<String>,
    /// Registered-script key to execute (exactly one of `script` / `key`).
    pub(crate) key: Option<String>,
    /// Caller-asserted partition key for per-partition fairness (Tier 5), single-tenant mode only.
    /// The `X-Partition-Key` header takes precedence over this field. **Ignored in trusted-header
    /// mode**, where the fairness key is the trusted tenant id (a caller cannot pick its bucket).
    #[serde(default)]
    pub(crate) partition: Option<String>,
    /// Raw context passed straight to `QuickJS` — never deserialized in Rust.
    #[serde(default = "default_context")]
    pub(crate) context: Box<RawValue>,
    /// Per-request configuration.
    #[serde(default)]
    pub(crate) config: RequestConfig,
}

/// Resolved script source — inline from the request body or shared from the registry.
#[derive(Debug)]
pub(crate) enum ScriptSource {
    /// Inline `script` field.
    Inline(String),
    /// Registered script resolved from `key`.
    Registered(Arc<str>),
}

impl ScriptSource {
    /// The script text.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Inline(source) => source.as_str(),
            Self::Registered(source) => source.as_ref(),
        }
    }
}

/// Per-request configuration sent by the caller.
///
/// Driver-backed capabilities carry no connection config here: the request names logical resources
/// in [`io`](Self::io) and the box forwards those names to the broker, which resolves the
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
    /// `$std` env/secrets context (omit to leave `$std.env`/`$std.secrets` empty).
    #[serde(default)]
    pub(crate) sys: Option<SysConfig>,
    /// Logical resources this invocation may reach — a plain allowlist of names (e.g.
    /// `["orders","cache"]`). The box is kind-blind: it forwards the names to the broker (which
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
    pub(crate) const fn any(&self) -> bool {
        !self.0.is_empty()
    }

    /// The allowlisted names. Passed to the engine as `CapabilitySet.io`: `io` is injected globally
    /// under `Profile::Full`, and `io.call(name, …)` is gated by this list (an unlisted name is
    /// rejected `RESOURCE_NOT_FOUND` before any egress).
    pub(crate) fn enabled_names(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }

    /// The names that must be resolved by the **broker** — every allowlisted name not bound
    /// box-direct in the global `local_resources` map. Box-direct names are served locally, so they
    /// are never sent to the broker (which would fail to resolve them).
    pub(crate) fn broker_names(&self, local: &HashMap<String, String>) -> Vec<String> {
        self.0
            .iter()
            .filter(|name| !local.contains_key(*name))
            .cloned()
            .collect()
    }
}

/// The broker session-open message: the flat list of broker-resolved resource names, the
/// per-execution deadline, and the request's trusted tenant id (so the broker scopes resolution to
/// that tenant's bindings). the broker resolves each name against its operator config.
pub(crate) fn wire_init(
    resources: Vec<String>,
    timeout: Duration,
    tenant: Option<&str>,
) -> WireInit {
    WireInit {
        resources,
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        // The trusted tenant id, sourced only from the trusted-header extractor (never the script).
        // `None` on the single-tenant/loopback path. Its **presence** is itself the multitenant
        // signal: the broker treats any tenant-scoped session as least-privilege-mandatory (the
        // `allow_privileged` opt-out is void), so the box carries no separate privilege flag. See
        // `docs/design/resource-egress.md` (least-privilege / trust model).
        tenant: tenant.map(str::to_owned),
        // The token (QUIC path) is attached by `connect_session` from the transport's auth
        // provider — the box-request layer never sees it.
        token: None,
    }
}

/// Returns a clone of the pre-allocated default context.
pub(crate) fn default_context() -> Box<RawValue> {
    DEFAULT_CONTEXT.clone()
}
