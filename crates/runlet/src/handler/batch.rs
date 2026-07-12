//! `POST /batch` fan-out: request/response types, admission + validation, and the per-item pipeline that runs each item through the shared single-execute machinery.

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::Instrument as _;
use uuid::Uuid;

use runlet_core::errors::{ErrorDebug, ErrorEnvelope};

use crate::identity::TrustedIdentity;

use super::{
    AppState, BatchItemCtx, Meta, RequestConfig, RespCfg, build_request_span, current_trace_id,
    default_context, emit_denied, enforce_auth, header_partition, record_identity_attrs,
    render_slots, request_error, resolve_identity, resolve_partition, run_after_phase,
    run_batch_item, run_before_phase, system_error_response,
};

// ===== POST /batch — independent per-item fan-out over the single-execute machinery =====

/// A `/batch` request body: an ordered list of independent items plus an optional
/// `before`/`shared`/`after` lifecycle. No atomicity, no cross-item ordering guarantee during
/// execution — only the results array preserves request order.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchRequest {
    /// The items to execute (validated for count/size before any admission).
    #[serde(default)]
    pub(crate) items: Vec<BatchItem>,
    /// Optional one-time setup phase run **alone before any item** (design D1/D2). Its returned
    /// `data`, merged over the `shared` seed, becomes the immutable shared context every item reads.
    /// A `before` failure is a barrier: the whole batch aborts non-200 and no item runs (RQ1/D3).
    #[serde(default)]
    pub(crate) before: Option<BatchItem>,
    /// Optional read-only seed object merged into the shared context (constants that need no fetch).
    /// Absent ⇒ the shared context is `before`'s output alone; both absent ⇒ no shared context is
    /// injected and the batch behaves exactly as today.
    #[serde(default)]
    pub(crate) shared: Option<Box<RawValue>>,
    /// Optional reduce phase run **alone after all items complete** (design D1/D2). It receives the
    /// order-preserving `results` (full per-item envelopes, RQ2); its returned `data` becomes the
    /// batch-level `summary`. An `after` failure is best-effort: HTTP 200 with `results` intact and a
    /// `meta.summary_error` (RQ1/D3).
    #[serde(default)]
    pub(crate) after: Option<BatchItem>,
}

/// One `/batch` item — the single-execute body shape plus an optional client `id` echoed back on its
/// result (D7). No per-item `partition`: fairness is keyed off the request's tenant/partition, shared
/// across all items so a caller cannot split its fairness bucket.
#[derive(Debug, Deserialize)]
pub(crate) struct BatchItem {
    /// Inline JavaScript source (exactly one of `script` / `key`).
    #[serde(default)]
    pub(crate) script: Option<String>,
    /// Registered-script key (exactly one of `script` / `key`).
    #[serde(default)]
    pub(crate) key: Option<String>,
    /// Raw context passed straight to `QuickJS`.
    #[serde(default = "default_context")]
    pub(crate) context: Box<RawValue>,
    /// Per-item configuration (capabilities, `io`).
    #[serde(default)]
    pub(crate) config: RequestConfig,
    /// Optional client correlation id, echoed on the result for subset-retry (D7).
    #[serde(default)]
    pub(crate) id: Option<String>,
}

/// The `/batch` response: order-preserving per-item envelopes + a batch-level summary.
///
/// `summary`/`summary_error` sit at the **top level**, peer to `results` (design RQ1) — the reduced
/// value is a primary product of the batch, not metadata about it. Both are omitted when absent, so
/// an unadorned batch response (no `after` phase) is byte-identical to the pre-lifecycle format.
#[derive(Debug, Serialize)]
pub(crate) struct BatchResponse {
    /// One rendered `{data, error, meta, id?}` envelope per item, in request order.
    pub(crate) results: Vec<Box<RawValue>>,
    /// The `after` phase's reduced value over `results` (design RQ1); omitted when no `after` ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<Box<RawValue>>,
    /// The classified error of a **failed** `after` phase (design RQ1/D3); the batch still responds
    /// `200` with `results` intact. Omitted when `after` was absent or succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary_error: Option<ErrorEnvelope>,
    /// Batch-level summary.
    pub(crate) meta: BatchMeta,
}

/// Batch-level summary metadata (per-item timing/metrics live in each `results[i].meta`).
#[derive(Debug, Serialize)]
pub(crate) struct BatchMeta {
    /// Number of items in the batch.
    pub(crate) items: usize,
    /// Items that executed successfully (engine success).
    pub(crate) ok: usize,
    /// Items that failed (rejected, engine error, or truncated).
    pub(crate) failed: usize,
    /// Wall-clock duration of the whole batch fan-out, milliseconds.
    pub(crate) duration_ms: u128,
    /// The batch correlation id — shared by every item's `meta.trace_id`.
    pub(crate) trace_id: String,
}

/// A fully-rendered batch item: its serialized `{data, error, meta, id?}` JSON plus whether it
/// counted as a success (for the batch `meta.ok/failed` summary).
pub(crate) struct RenderedItem {
    /// Serialized item envelope (owned, valid JSON text).
    pub(crate) body: String,
    /// `true` iff the item executed successfully (engine success, not a rejection/error/truncation).
    pub(crate) ok: bool,
}

impl RenderedItem {
    /// Serialized byte length — the unit the D6 total-response cap sums.
    pub(crate) const fn bytes(&self) -> usize {
        self.body.len()
    }
}

/// A serialized system-error item: `{ data: null, error, meta, id? }`.
#[derive(Serialize)]
pub(crate) struct ItemErrorEnvelope<'a> {
    /// Always `null` on a per-item system error.
    pub(crate) data: Option<()>,
    /// The structured error envelope.
    pub(crate) error: &'a ErrorEnvelope,
    /// Per-item metadata.
    pub(crate) meta: &'a Meta,
    /// The echoed client id, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<&'a str>,
}

/// A serialized success item: `{ data, error, meta, id? }` (data/error borrowed from the JS output).
#[derive(Serialize)]
pub(crate) struct ItemSuccessEnvelope<'a> {
    /// The JS handler's `data` (zero-copy borrow).
    pub(crate) data: &'a RawValue,
    /// The JS handler's `error` (zero-copy borrow; the application-level error passthrough).
    pub(crate) error: &'a RawValue,
    /// Per-item metadata.
    pub(crate) meta: &'a Meta,
    /// The echoed client id, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<&'a str>,
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
pub(crate) async fn run_batch(
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
    let identity = match resolve_identity(&state, &headers, &trace_id).await {
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
            )
            .await;
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
    if let Err(rejected) = validate_batch(&state, &items, identity.as_ref(), &trace_id).await {
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
pub(crate) struct BatchEnv<'a> {
    /// Shared application state.
    pub(crate) state: &'a AppState,
    /// The request's trusted identity (shared across every phase), if any.
    pub(crate) identity: Option<&'a TrustedIdentity>,
    /// The request's fairness/cache key (shared across every phase).
    pub(crate) partition: Option<&'a str>,
    /// The batch correlation id.
    pub(crate) trace_id: &'a str,
    /// Response-shaping policy (drives the `before` barrier's projected status).
    pub(crate) cfg: RespCfg,
}

/// Phase 2 — the bounded concurrent item fan-out (unchanged behavior). Bounds this batch's
/// concurrency to its fair share so it cannot monopolize the pool (D2): the per-partition ceiling
/// when fairness is on, else the global bulkhead capacity. Items beyond the ceiling queue on this
/// gate rather than fast-failing (unlike single `/execute`). Each item reads the immutable shared
/// context. Returns positional slots so the results array preserves request order regardless of
/// completion order (a slot left empty by a panicked task is filled defensively downstream).
pub(crate) async fn fan_out_items(
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
pub(crate) fn batch_ceiling(state: &AppState) -> usize {
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
pub(crate) async fn validate_batch(
    state: &AppState,
    items: &[BatchItem],
    identity: Option<&TrustedIdentity>,
    trace_id: &str,
) -> Result<(), Box<AxumResponse>> {
    if items.is_empty() {
        emit_denied(state, identity, trace_id, "EMPTY_BATCH", None).await;
        return Err(Box::new(batch_level_error(
            "EMPTY_BATCH",
            "batch must contain at least one item".to_owned(),
            trace_id,
        )));
    }
    if items.len() > state.batch.max_items {
        emit_denied(state, identity, trace_id, "BATCH_TOO_LARGE", None).await;
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
        emit_denied(state, identity, trace_id, "BATCH_INPUT_TOO_LARGE", None).await;
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
pub(crate) fn batch_item_input_bytes(item: &BatchItem) -> usize {
    let script = item.script.as_ref().map_or(0, String::len);
    script.saturating_add(item.context.get().len())
}

/// A batch-level `400` response (malformed body / caps): the single `{data:null, error, meta}`
/// envelope with the batch trace id — batch-level rejections are non-200 (design D4).
pub(crate) fn batch_level_error(code: &str, message: String, trace_id: &str) -> AxumResponse {
    system_error_response(
        request_error(code, message),
        400,
        Meta::new(trace_id.to_owned(), 0, 0, 0),
    )
}

/// The structured `400` for a `/batch` body that failed to parse (bad JSON / wrong types / oversize).
pub(crate) fn malformed_batch_response(
    state: &AppState,
    rejection: &JsonRejection,
) -> AxumResponse {
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
