//! Batch per-item support: per-item authz/quota gates, item rendering, and the optional `before`/`after` lifecycle phases.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use serde_json::value::RawValue;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use tokio::task;
use tracing::warn;

use runlet_core::engine::{EngineError, ExecOutcome};
use runlet_core::errors::ErrorEnvelope;
use runlet_core::host::Outcome;
use runlet_core::metrics::Metrics;
use runlet_core::sandbox;

use crate::authz::authorize_capabilities;
use crate::broker::connect_session;
use crate::identity::TrustedIdentity;
use crate::quota::QuotaGuard;

use super::{
    AppState, BatchItem, EgressMetrics, Envelope, ExecuteBlocking, ItemErrorEnvelope,
    ItemSuccessEnvelope, Meta, RenderedItem, RequestConfig, RespCfg, base_error_meta, emit_denied,
    emit_executed, engine_error_outcome, execute_blocking, quota_exceeded_envelope,
    record_capability_latencies, request_error, requested_capabilities, resolve_script,
    session_error_envelope, wire_init,
};

/// Per-item member-capability authz (trusted mode, D5). `None` = permitted (or not gated); `Some`
/// carries the `ENTITLEMENT_REQUIRED` envelope. Emits the denied audit on rejection.
pub(crate) async fn batch_item_authz(
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
            )
            .await;
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
pub(crate) async fn batch_item_quota(
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
            )
            .await;
            Err(Box::new(quota_exceeded_envelope(&exceeded)))
        }
    }
}

/// Renders one item's execution outcome (mirrors [`build_response`]): emits the per-item usage/audit
/// events + records per-item metrics, then serializes the `{data, error, meta, id?}` envelope.
pub(crate) async fn render_executed_item(
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
                    emit_executed(state, identity, &meta, "success").await;
                    metrics.record_success();
                    render_success_item(&js_json, &meta, id, state.resp_cfg())
                }
                ExecOutcome::Error(engine_err) => {
                    let outcome = engine_error_outcome(&engine_err);
                    emit_executed(state, identity, &meta, outcome).await;
                    metrics.record_engine_error(&engine_err);
                    render_engine_error_item(engine_err, &meta, id, state.resp_cfg())
                }
            }
        }
        Ok((Err(engine_err), _backend)) => {
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome).await;
            metrics.record_engine_error(&engine_err);
            render_engine_error_item(engine_err, &base_meta, id, state.resp_cfg())
        }
        Err(join_err) => {
            let engine_err = EngineError::Internal(format!("task panicked: {join_err}"));
            let outcome = engine_error_outcome(&engine_err);
            emit_executed(state, identity, &base_meta, outcome).await;
            metrics.record_engine_error(&engine_err);
            render_engine_error_item(engine_err, &base_meta, id, state.resp_cfg())
        }
    }
}

/// Serializes a success item from the JS `{data, error}` output + meta + id. A JS output that does
/// not parse is rendered as a `MALFORMED_RESPONSE` error item instead (mirrors [`success_response`]).
pub(crate) fn render_success_item(
    js_json: &str,
    meta: &Meta,
    id: Option<&str>,
    cfg: RespCfg,
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
                    .into_envelope(cfg.error_debug, cfg.timeout_retryable);
            render_error_item(&envelope, meta, id)
        }
    }
}

/// Serializes an engine-error item, logging the raw cause server-side keyed by `trace_id` (mirrors
/// [`engine_error_response`]). A `/batch` item's classification rides its rendered envelope inside
/// the `200` batch (design D4) — the projected HTTP status applies to single `/execute` only.
pub(crate) fn render_engine_error_item(
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
pub(crate) fn render_error_item(
    error: &ErrorEnvelope,
    meta: &Meta,
    id: Option<&str>,
) -> RenderedItem {
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
pub(crate) fn fallback_item_body(trace_id: &str) -> String {
    format!(
        "{{\"data\":null,\"error\":{{\"code\":\"INTERNAL_ERROR\"}},\"meta\":{{\"trace_id\":{trace_id:?}}}}}"
    )
}

// ===== Batch lifecycle: before → items → after (design D1/D2, RQ1–RQ3) =====

/// Merges framework-supplied read-only `reserved` keys into a JSON **object** `context`, returning the
/// serialized result. Values are kept as `RawValue` so number fidelity survives the round-trip; a
/// reserved key overwrites a same-named field the caller declared (the framework value wins). Returns
/// `None` when `context` is not a JSON object (the caller decides how to surface that).
pub(crate) fn context_with_reserved(
    context: &RawValue,
    reserved: &[(&str, &str)],
) -> Option<String> {
    let mut map: BTreeMap<String, Box<RawValue>> = serde_json::from_str(context.get()).ok()?;
    for &(key, value) in reserved {
        let _prev = map.insert(
            key.to_owned(),
            RawValue::from_string(value.to_owned()).ok()?,
        );
    }
    serde_json::to_string(&map).ok()
}

/// Builds the immutable shared context from the `shared` seed and `before`'s returned `data` (design
/// D4/RQ3). `None` ⇒ neither was supplied (no injection, byte-identical to the pre-lifecycle path).
/// When both are JSON objects they shallow-merge with `before`'s data winning; otherwise `before`'s
/// data (when present) is the shared context verbatim and the seed is ignored.
pub(crate) fn build_shared_context(
    seed: Option<&RawValue>,
    before_data: Option<&RawValue>,
) -> Option<String> {
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

/// Inputs for running one batch item (grouped to stay within the argument-count lint).
pub(crate) struct BatchItemCtx<'a> {
    /// Shared application state.
    pub(crate) state: &'a AppState,
    /// This batch's fair-share concurrency gate.
    pub(crate) gate: Arc<Semaphore>,
    /// The request's trusted identity (shared across items), if any.
    pub(crate) identity: Option<&'a TrustedIdentity>,
    /// The request's fairness/cache key (shared across items).
    pub(crate) partition: Option<&'a str>,
    /// The batch correlation id (every item's `meta.trace_id`).
    pub(crate) trace_id: &'a str,
    /// The item to run.
    pub(crate) item: BatchItem,
    /// The immutable shared context built by the `before` phase (serialized JSON), or `None` when the
    /// batch has no lifecycle. When present it is injected read-only into the item's context object
    /// under the reserved `shared` key (design D4/RQ3); the `Arc` is shared, never cloned per item.
    pub(crate) shared: Option<Arc<str>>,
}

/// Runs one batch item through the same per-request machinery as `/execute`, rendering a per-item
/// envelope instead of an HTTP status: resolve script → validate size → per-item authz (D5) →
/// per-item quota debit (D5) → open session → admit (queue) → execute → render. Every security gate
/// is the exact function `/execute` uses, evaluated per item — never once for the batch.
#[expect(
    clippy::too_many_lines,
    reason = "linear per-item pipeline mirroring run_execute: resolve → validate → authz → quota → session → admit → execute → render"
)]
pub(crate) async fn run_batch_item(ctx: BatchItemCtx<'_>) -> RenderedItem {
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
            emit_denied(state, identity, trace_id, "SCRIPT_NOT_FOUND", None).await;
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
        emit_denied(state, identity, trace_id, "INPUT_TOO_LARGE", None).await;
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
            emit_denied(state, identity, trace_id, "INVALID_CONTEXT", None).await;
            let meta = base_error_meta(
                trace_id,
                script_bytes,
                context_bytes,
                key.as_deref(),
                partition,
            );
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
    if let Some(envelope) = batch_item_authz(state, identity, &config, trace_id).await {
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
    let _item_quota = match batch_item_quota(state, identity, trace_id).await {
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

    // Open the broker session only for broker-resolved names (box-direct names are served locally).
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
        // Same trusted tenant fed to the broker's `WireInit`; forwarded box-direct as a header.
        tenant: identity.and_then(|trusted| trusted.tenant.as_deref().map(str::to_owned)),
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
    render_executed_item(state, identity, result, base_meta, id_ref).await
}
