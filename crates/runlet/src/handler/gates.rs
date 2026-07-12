//! Request admission and security gates: script resolution, the bulkhead/partition admission, edge-credential auth, trusted-identity resolution, member authorization, and per-tenant quota.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::response::Response as AxumResponse;
use serde_json::json;
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

use runlet_core::errors::{ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
use runlet_core::registry::ScriptRegistry;
use runlet_wire::ct_eq;

use crate::authz::authorize_capabilities;
use crate::identity::TrustedIdentity;
use crate::quota::{QuotaExceeded, QuotaGuard};

use super::{
    AppState, Meta, RequestConfig, RespCfg, ScriptSource, emit_denied, projected_error_response,
    system_error_response,
};

/// Resolves the script source for a request: exactly one of `script` / `key` must be
/// present; a `key` is looked up in the registry.
///
/// # Errors
///
/// Returns the HTTP status + envelope for the violation (boxed — the happy path
/// shouldn't carry the envelope's size): 400 `SCRIPT_XOR_KEY` when not exactly one of
/// the two is present, 404 `SCRIPT_NOT_FOUND` for an unknown key.
pub(crate) fn resolve_script(
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
pub(crate) fn malformed_request_response(
    state: &AppState,
    rejection: &JsonRejection,
) -> AxumResponse {
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
pub(crate) fn overloaded_response(meta: Meta, cfg: RespCfg) -> AxumResponse {
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
pub(crate) fn partition_overloaded_response(meta: Meta, cfg: RespCfg) -> AxumResponse {
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
pub(crate) enum Admission {
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
pub(crate) fn admit(
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
pub(crate) fn acquire_permits(state: &AppState, partition: Option<&str>) -> Admission {
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
pub(crate) fn base_error_meta(
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
pub(crate) fn header_partition(headers: &HeaderMap) -> Option<String> {
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
pub(crate) async fn resolve_identity(
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
        )
        .await;
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
        )
        .await;
        return Err(Box::new(identity_rejected(
            trace_id,
            "SUSPENDED_FORBIDDEN",
            "a suspended principal may not execute code",
        )));
    }
    if identity.tenant.is_none() {
        emit_denied(state, Some(&identity), trace_id, "TENANT_REQUIRED", None).await;
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
        )
        .await;
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
pub(crate) fn identity_rejected(trace_id: &str, code: &str, message: &str) -> AxumResponse {
    let meta = Meta::new(trace_id.to_owned(), 0, 0, 0);
    system_error_response(request_error(code, message.to_owned()), 403, meta)
}

/// The fairness + cache key for a request. In trusted mode it is the trusted tenant id and the
/// caller-asserted source is ignored; otherwise it is the caller-asserted source (single-tenant).
pub(crate) fn resolve_partition(
    identity: Option<&TrustedIdentity>,
    caller_asserted: Option<String>,
) -> Option<String> {
    identity.map_or(caller_asserted, |id| id.tenant.clone())
}

/// The capabilities a request exercises — the flat logical resource names in `config.io` plus the
/// in-engine `http`/`s3` when their config is present. Used by the member-authz gate (which now
/// keys `capability_entitlements` by logical resource name, not kind).
pub(crate) fn requested_capabilities(config: &RequestConfig) -> Vec<&str> {
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
pub(crate) async fn enforce_member_authz(
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
            )
            .await;
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
pub(crate) async fn enforce_quota<F: FnOnce() -> Meta>(
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
            )
            .await;
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
pub(crate) fn quota_exceeded_envelope(exceeded: &QuotaExceeded) -> ErrorEnvelope {
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
pub(crate) fn quota_exceeded_response(
    exceeded: &QuotaExceeded,
    meta: Meta,
    cfg: RespCfg,
) -> AxumResponse {
    projected_error_response(quota_exceeded_envelope(exceeded), meta, cfg)
}

/// Enforces the optional `/execute` bearer gate. Returns `Some(401)` when a token is
/// configured and the request doesn't present a matching one; `None` when auth passes or no
/// token is configured (auth handled upstream / loopback bind).
pub(crate) fn enforce_auth(state: &AppState, headers: &HeaderMap) -> Option<AxumResponse> {
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
pub(crate) fn request_authorized(headers: &HeaderMap, expected: &str) -> bool {
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
pub(crate) fn unauthorized_response() -> AxumResponse {
    let trace_id = Uuid::new_v4().to_string();
    let envelope = request_error("UNAUTHORIZED", "missing or invalid bearer token".to_owned());
    system_error_response(envelope, 401, Meta::new(trace_id, 0, 0, 0))
}

/// Builds a `request`-category envelope (the caller's fault, never retryable).
pub(crate) fn request_error(code: &str, message: String) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCategory::Request,
        ErrorSource::Request,
        code.to_owned(),
        false,
        ErrorOwner::Caller,
    )
    .with_message(message)
}
