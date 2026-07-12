//! `POST /batch` driven directly through the handler: order preservation + `id` echo, per-item
//! isolation, partial failure, batch-level caps (empty / too-many / xor-key item), the D6
//! response-size truncation, and — the D5 GraphQL-batch-attack guard — per-item authorization.
//! Egress is not wired (no broker), so items run deterministic scripts.

use super::{AppState, BatchItem, BatchRequest, RequestConfig, RequestIo, TrustedRuntime, batch};
use crate::broker::BrokerTransport;
use crate::config::{BatchConfig, TrustedHeaders};
use crate::quota::{PlanLimit, TenantQuota};
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

/// Builds an app state with a small warm pool, no broker, and the given trusted runtime.
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
        transport: BrokerTransport::None,
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
        assert_eq!(
            body["results"][index]["data"], 10,
            "item read before's output"
        );
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
    let (status, body) = run_full(&app, HeaderMap::new(), items, before, Some(seed), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"][0]["data"]["seed"], 9, "seed value visible");
    assert_eq!(
        body["results"][0]["data"]["before"], 1,
        "before value visible"
    );
    assert_eq!(
        body["results"][0]["data"]["k"], 2,
        "before wins the collision"
    );
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
    assert!(
        !body["error"].is_null(),
        "the barrier carries the before error"
    );
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
    let after_err = Some(phase(
        "function handler(){ throw new Error('reduce failed'); }",
    ));
    let (status, body) = run_full(&app, HeaderMap::new(), mk_items(), None, None, after_err).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a failed after does not fail the batch"
    );
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
    assert_ne!(
        status,
        StatusCode::OK,
        "quota-denied before aborts the batch"
    );
    assert_eq!(
        body["error"]["code"], "QUOTA_EXCEEDED",
        "before debits quota like an item"
    );
    assert!(
        body.get("results").is_none(),
        "no item runs past the barrier"
    );
}

/// 4.7b — Gates: I/O in `before` is gated exactly as for an item — with no broker, a `before`
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
    assert_ne!(
        status,
        StatusCode::OK,
        "no broker ⇒ before I/O fails closed"
    );
    assert_eq!(body["error"]["code"], "EGRESS_UNAVAILABLE");
    assert!(body.get("results").is_none(), "the barrier runs no items");
}
