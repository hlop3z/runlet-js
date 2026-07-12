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

impl CapturingSink {
    /// Serialize + capture one event's JSON line (shared by the precious and log paths).
    fn capture(&self, event: &Event) {
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        if let Ok(mut lines) = self.lines.lock() {
            lines.push(json);
        }
    }
}

impl Sink for CapturingSink {
    fn record(&self, event: Event) -> core::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.capture(&event) })
    }

    fn record_log(&self, event: Event) {
        // Diagnostic log events land in the same capture list (uniform for assertions).
        self.capture(&event);
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
        .filter(|line| line.contains("\"decision\":\"denied\"") && line.contains("QUOTA_EXCEEDED"))
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
