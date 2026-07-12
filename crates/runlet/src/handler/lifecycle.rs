//! Batch `before`/`after` lifecycle phases: the shared-context and summary machinery run
//! once per batch (outside the per-item slots), plus the slot-rendering helpers.

use std::sync::Arc;
use std::time::Instant;

use axum::response::Response as AxumResponse;
use serde_json::value::RawValue;
use tokio::runtime::Handle;
use tokio::task;

use runlet_core::engine::{EngineError, ExecOutcome};
use runlet_core::errors::{ErrorCategory, ErrorEnvelope, ErrorOwner, ErrorSource};
use runlet_core::host::Outcome;
use runlet_core::metrics::Metrics;
use runlet_core::sandbox;

use crate::broker::connect_session;
use crate::identity::TrustedIdentity;

use super::{
    AppState, BatchEnv, BatchItem, EgressMetrics, Envelope, ExecuteBlocking, Meta, RAW_NULL,
    RenderedItem, base_error_meta, batch_item_authz, batch_item_quota, build_shared_context,
    context_with_reserved, emit_denied, emit_executed, engine_error_outcome, execute_blocking,
    projected_error_response, record_capability_latencies, render_error_item, request_error,
    resolve_script, session_error_envelope, wire_init,
};

/// The outcome of a `before`/`after` lifecycle invocation: the extracted handler `data` on success
/// (used to build the shared context / the `summary`), or a classified error (a gate rejection or an
/// engine error) that becomes the `before` barrier response or the `after` `summary_error`.
pub(crate) enum LifecyclePhase {
    /// Handler succeeded; carries its returned `data` (the reproducible product, RQ2).
    Success(Box<RawValue>),
    /// A gate rejection or engine error, already classified into a wire envelope.
    Failure(ErrorEnvelope),
}

/// Inputs for one lifecycle invocation (grouped to stay within the argument-count lint). Mirrors
/// [`BatchItemCtx`] but carries the already-merged `context_json` (the phase's own context for
/// `before`; the `results`/`shared` reserved keys merged in for `after`) and returns a
/// [`LifecyclePhase`] rather than a rendered envelope.
pub(crate) struct LifecycleCtx<'a> {
    /// Shared application state.
    pub(crate) state: &'a AppState,
    /// The request's trusted identity (shared with the items), if any.
    pub(crate) identity: Option<&'a TrustedIdentity>,
    /// The request's fairness/cache key (shared with the items).
    pub(crate) partition: Option<&'a str>,
    /// The batch correlation id.
    pub(crate) trace_id: &'a str,
    /// The phase invocation (script/key/config; its `context` field is superseded by `context_json`).
    pub(crate) item: BatchItem,
    /// The fully-merged context handed to the handler.
    pub(crate) context_json: String,
}

/// Runs one `before`/`after` phase through the **same per-invocation gates an item gets** (resolve →
/// size → authz → quota debit → session → admit → execute), returning the structured outcome (design
/// D2). A lifecycle phase is never a cheaper unit of admission/quota/billing than an item; it runs
/// alone (sequentially, outside the fan-out) so it needs no fair-share slot, only the global bulkhead
/// permit that protects the blocking threads.
pub(crate) async fn run_lifecycle_phase(ctx: LifecycleCtx<'_>) -> LifecyclePhase {
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
            emit_denied(state, identity, trace_id, "SCRIPT_NOT_FOUND", None).await;
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
        emit_denied(state, identity, trace_id, "INPUT_TOO_LARGE", None).await;
        return LifecyclePhase::Failure(request_error(code, message));
    }

    // Same per-invocation member-capability authz + quota debit an item gets (RQ3): a lifecycle phase
    // counts against quota, so a batch with a `before`/`after` is never cheaper than the equivalent
    // single requests. The quota guard is held across this phase's execution.
    if let Some(envelope) = batch_item_authz(state, identity, &config, trace_id).await {
        return LifecyclePhase::Failure(envelope);
    }
    let _quota = match batch_item_quota(state, identity, trace_id).await {
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
                emit_denied(state, identity, trace_id, "EGRESS_UNAVAILABLE", None).await;
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
    let base_meta = Meta::new(
        trace_id.to_owned(),
        script_bytes,
        context_bytes,
        exec_time_us,
    )
    .with_partition(partition.map(str::to_owned));
    lifecycle_outcome(state, identity, result, base_meta).await
}

/// Classifies a lifecycle phase's execution outcome (mirrors [`render_executed_item`] but extracts the
/// handler `data` instead of rendering an envelope): emits the per-invocation usage/audit events +
/// records metrics on every path, then yields `Success(data)` or `Failure(envelope)`.
pub(crate) async fn lifecycle_outcome(
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
                    emit_executed(state, identity, &meta, "success").await;
                    metrics.record_success();
                    match serde_json::from_str::<Envelope<'_>>(&js_json) {
                        Ok(env) => LifecyclePhase::Success(
                            RawValue::from_string(env.data.get().to_owned())
                                .unwrap_or_else(|_err| RAW_NULL.clone()),
                        ),
                        Err(parse_err) => LifecyclePhase::Failure(
                            EngineError::Malformed(format!(
                                "malformed handler response: {parse_err}"
                            ))
                            .into_envelope(cfg.error_debug, cfg.timeout_retryable),
                        ),
                    }
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome).await;
                    metrics.record_engine_error(&engine_err);
                    LifecyclePhase::Failure(
                        engine_err.into_envelope(cfg.error_debug, cfg.timeout_retryable),
                    )
                }
            }
        }
        Ok((Err(engine_err), _egress)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome).await;
            metrics.record_engine_error(&engine_err);
            LifecyclePhase::Failure(
                engine_err.into_envelope(cfg.error_debug, cfg.timeout_retryable),
            )
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome).await;
            metrics.record_engine_error(&engine_err);
            LifecyclePhase::Failure(
                engine_err.into_envelope(cfg.error_debug, cfg.timeout_retryable),
            )
        }
    }
}

/// Phase 1 — the `before` barrier. Runs `before` (when present) alone, then builds the immutable
/// shared context from the `shared` seed + `before`'s `data`, enforcing the `max_shared_bytes` cap.
/// Returns the shared context (`None` ⇒ no lifecycle, inject nothing). Any `before` failure — or an
/// over-cap shared context — becomes a non-200 batch-level barrier response (RQ1/D3): no item runs.
pub(crate) async fn run_before_phase(
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
        )
        .await;
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
pub(crate) async fn run_after_phase(
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
pub(crate) fn render_slots(
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
pub(crate) fn internal_error_item(trace_id: &str) -> RenderedItem {
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
pub(crate) fn truncated_item(trace_id: &str) -> RenderedItem {
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
