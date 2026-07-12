//! `/execute` HTTP status = projection of the outcome (design D1/D5): a null-error success is
//! `200`; a handler-returned error is never `2xx` — it parks at `422` unless the handler opts
//! into retry (`retryable: true ⇒ 503` + `Retry-After`); the body is passed through verbatim.
//! Also covers the engine-error projections reachable without a wired broker (oversize `413`,
//! syntax `422`).

use super::{AppState, ExecRequest, RequestConfig, default_context, execute};
use crate::broker::BrokerTransport;
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

/// A non-trusted (single-tenant, no broker) app state with a small warm pool.
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
        transport: BrokerTransport::None,
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
    let script = "function handler(){ return json(null, { message: 'later', retryable: true }); }";
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
    let script = "function handler(){ return json(null, { message: 'nope', retryable: false }); }";
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
