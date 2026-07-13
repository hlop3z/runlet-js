//! HTTP handler for the `/execute` endpoint: the single-execute path plus the shared
//! router state. The batch fan-out, response assembly, security gates, telemetry, and data
//! types live in submodules; this module owns `execute`/`run_execute`/`execute_blocking` and
//! re-exports the submodule surface used by the router (`main.rs`) and the tests.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::HeaderMap;
use axum::response::Response as AxumResponse;
use tokio::runtime::Handle;
use tokio::task;
use tracing::Instrument as _;

use runlet_core::engine::{EngineError, LogLevel};
use runlet_core::host::{CapabilitySet, Invocation, LogicHost, Outcome};
use runlet_core::sandbox;
use runlet_wire::{Egress, MeteredEgress};

use crate::broker::{BrokerEgress, SessionConn, connect_session};
use crate::local_io::BoxEgress;

mod batch;
mod batch_items;
mod gates;
mod lifecycle;
mod response;
mod telemetry;
mod types;

// Submodule surface re-exported for the router (`main.rs`) and this module's single-execute path.
pub(crate) use batch::{
    BatchEnv, BatchItem, ItemErrorEnvelope, ItemSuccessEnvelope, RenderedItem, batch,
};
pub(crate) use batch_items::BatchItemCtx;
pub(crate) use batch_items::{
    batch_item_authz, batch_item_quota, build_shared_context, context_with_reserved,
    render_error_item, run_batch_item,
};
pub(crate) use gates::{
    admit, base_error_meta, enforce_auth, enforce_member_authz, enforce_quota, header_partition,
    malformed_request_response, quota_exceeded_envelope, request_error, requested_capabilities,
    resolve_identity, resolve_partition, resolve_script,
};
pub(crate) use lifecycle::{render_slots, run_after_phase, run_before_phase};
pub(crate) use response::{
    EgressMetrics, Envelope, Meta, engine_error_response, io_count, projected_error_response,
    session_error_envelope, session_error_response, success_response, system_error_response,
};
pub(crate) use telemetry::{
    build_request_span, build_response, current_trace_id, emit_denied, emit_executed,
    engine_error_outcome, metrics, record_capability_latencies, record_identity_attrs,
};
pub(crate) use types::{
    AppState, ExecRequest, RAW_NULL, RequestConfig, RespCfg, ScriptSource, TrustedRuntime,
    default_context, wire_init,
};

// Names referenced only from this module's #[cfg(test)] submodules (via `super::`). Gated so the
// non-test bin build doesn't flag them as unused re-exports.
#[cfg(test)]
pub(crate) use batch::BatchRequest;
#[cfg(test)]
pub(crate) use gates::request_authorized;
#[cfg(test)]
pub(crate) use response::{Response, SystemErrorResponse};
#[cfg(test)]
pub(crate) use runlet_core::engine::{Effect, LogEntry};
#[cfg(test)]
pub(crate) use runlet_core::errors::ErrorEnvelope;
#[cfg(test)]
pub(crate) use serde_json::value::RawValue;
#[cfg(test)]
pub(crate) use telemetry::LogPolicy;
#[cfg(test)]
pub(crate) use types::RequestIo;

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
pub(crate) async fn run_execute(
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
    let identity = match resolve_identity(&state, &headers, &trace_id).await {
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
            )
            .await;
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
    if let Some(rejected) =
        enforce_member_authz(&state, identity.as_ref(), &config, &trace_id).await
    {
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
            )
            .await;
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
        )
        .await;
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
    })
    .await
    {
        Ok(guard) => guard,
        Err(rejected) => {
            state.metrics.record_rejection();
            return *rejected;
        }
    };

    // Open the broker egress session only for names the broker must resolve — every allowlisted
    // name **not** bound box-direct in the global `local_resources` map (D8). A box-direct-only
    // request opens no broker session. The box holds no credentials: it sends the broker names + the
    // trusted tenant id; the broker resolves them within that tenant's binding set. An unknown/
    // out-of-tenant name (400), or an unreachable/absent broker (503), is rejected here — before
    // admission.
    let broker_names = config.io.broker_names(&state.local_resources);
    let session = if broker_names.is_empty() {
        None
    } else {
        let init = wire_init(
            broker_names,
            engine_cfg.timeout(),
            tenant.as_deref(),
            identity.as_ref().and_then(|id| id.user.as_deref()),
        );
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
                )
                .await;
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
            emit_denied(&state, identity.as_ref(), &trace_id, "OVERLOADED", None).await;
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
        // Same trusted tenant fed to the broker's `WireInit`; forwarded box-direct as a header.
        tenant: identity
            .as_ref()
            .and_then(|id| id.tenant.as_deref().map(str::to_owned)),
        // Trusted acting subject, forwarded box-direct as the `X-Runlet-Actor` header (who).
        actor: identity
            .as_ref()
            .and_then(|id| id.user.as_deref().map(str::to_owned)),
    })
    .await;

    // Execution finished — free the bulkhead + per-partition permits for the next request.
    drop(permit);
    drop(partition_permit);

    let exec_time_us = start.elapsed().as_micros();
    let base_meta = Meta::new(trace_id, script_bytes, context_bytes, exec_time_us)
        .with_key(key)
        .with_partition(partition);
    build_response(result, base_meta, cfg, &state, identity.as_ref()).await
}

/// Inputs for the shared blocking-execution core (grouped so the call sites and the
/// `spawn_blocking` closure stay within the argument-count lint).
pub(crate) struct ExecuteBlocking {
    /// The callable logic host (cloned per invocation; `Arc`-backed).
    host: LogicHost,
    /// Runtime handle to drive the broker socket I/O via `block_on` on the blocking thread.
    handle: Handle,
    /// Per-execution wall-clock budget bounding every egress round-trip.
    timeout: Duration,
    /// The pre-connected broker session, when the request named broker-resolved resources.
    session: Option<SessionConn>,
    /// Box-direct local egress bindings (name → loopback URL), consulted before the broker (D8).
    local_resources: Arc<HashMap<String, String>>,
    /// The request's trusted tenant id, forwarded to a box-direct loopback service as the
    /// `X-Runlet-Tenant` header (the box-direct analogue of the broker's `WireInit.tenant`).
    /// `None` on the single-tenant/non-trusted path — no header is then emitted.
    tenant: Option<String>,
    /// The request's trusted acting subject, forwarded to a box-direct loopback service as the
    /// `X-Runlet-Actor` header (the *who* companion to `tenant`'s *where*). Sourced from
    /// `TrustedIdentity.user`; `None` on the single-tenant/non-trusted path — no header is then emitted.
    actor: Option<String>,
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
/// and each `/batch` item. Wraps the pre-connected broker session as the egress, runs the
/// invocation under the full-capability profile, then drains the session's driver metrics (the
/// round-trips + drain `block_on` must run on the `spawn_blocking` thread, never a runtime worker).
pub(crate) async fn execute_blocking(
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
        tenant,
        actor,
    } = params;
    task::spawn_blocking(move || -> (Result<Outcome, EngineError>, EgressMetrics) {
        // The broker session (if any) is wrapped as a `BrokerEgress`, then composed with the
        // box-direct bindings into a single `BoxEgress` (D8): a listed local name resolves
        // box-direct, everything else forwards to the broker. The `io` port is wired whenever the
        // request named any resource; a box-direct-only request opened no broker session.
        let broker = session.map(|conn| Arc::new(BrokerEgress::new(conn, handle.clone(), timeout)));
        let box_egress = config.io.any().then(|| {
            Arc::new(BoxEgress::new(
                Arc::clone(&local_resources),
                local_client.clone(),
                handle.clone(),
                timeout,
                broker,
                tenant,
                actor,
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod log_mirror_tests;

#[cfg(test)]
mod request_io_tests;

#[cfg(test)]
mod partition_tests;

#[cfg(test)]
mod trusted_pipeline_tests;

#[cfg(test)]
mod batch_tests;

#[cfg(test)]
mod execute_status_tests;

#[cfg(test)]
mod fail_closed_envelope_tests;
