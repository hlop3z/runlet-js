//! Box-direct local egress (byo-capabilities D8/D9).
//!
//! `io.call(name, action, payload)` for a name the operator declared in the global
//! `local_resources` map resolves **box-direct**: the box POSTs the identical `{action, payload}`
//! envelope a broker would receive in `WireCall` to the configured co-located loopback endpoint,
//! using a shared `reqwest` client — no broker session, no credentials. Any other named name
//! forwards to the broker ([`BrokerEgress`]). Both paths run through the same capability-mux
//! invariants (the request allowlist, `meta.io.<name>` metering, the execution deadline,
//! fail-closed).
//!
//! The logical name is a stable indirection: a service can be moved between a box-direct binding
//! and broker resolution with **zero** script change, because the on-the-wire envelope is identical
//! (D9). Box-direct targets are restricted to loopback/private addresses by the boot guard
//! (`config.rs::check_local_resources`); the script only ever sees a logical name, so there is no
//! SSRF surface.

use std::collections::{BTreeMap, HashMap};
use std::mem;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use runlet_wire::{BackendMetrics, Egress, EgressError, ErrorOwner, MeteredEgress};
use serde::Serialize;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::broker::BrokerEgress;

/// Out-of-band header carrying the request's trusted tenant id on a box-direct POST, matching the
/// `x-runlet-*` trusted-header family. Emitted only when a trusted tenant is present.
const TENANT_HEADER: &str = "x-runlet-tenant";

/// Out-of-band header carrying the request's trusted acting subject (the *who*, companion to the
/// tenant's *where*) on a box-direct POST. The bare subject only — never principal kind, roles, or any
/// other identity field. Emitted only when a trusted subject is present.
const ACTOR_HEADER: &str = "x-runlet-actor";

/// Per-op metric for one box-direct local call, surfaced in `meta.io.<name>` exactly like the
/// broker's per-capability metrics (so box-direct and broker resolution meter identically).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalIoMetric {
    /// Action verb (`"query"`, `"send"`, …).
    action: String,
    /// Duration in microseconds.
    duration_us: u128,
    /// Request envelope size in bytes.
    request_bytes: usize,
    /// HTTP status of the local response (`0` if the call failed before a response).
    status: u16,
}

/// The box-direct call envelope — the **identical** `{action, payload}` shape a broker receives in
/// `WireCall` (the `name` is the binding key, not part of the body), so a logical name is a stable
/// indirection across box-direct and broker resolution (D9). `payload` is the script's
/// JSON-encoded args carried verbatim as a string field, matching `WireCall`'s wire form.
#[derive(Serialize)]
struct LocalCallEnvelope<'a> {
    /// Action verb, opaque to the box.
    action: &'a str,
    /// The script's JSON-encoded arguments (untrusted), carried verbatim.
    payload: &'a str,
}

/// An [`Egress`] that resolves each `io.call` name **box-direct** to a co-located loopback endpoint
/// (when the operator declared one in the global `local_resources` map) or forwards it to the
/// broker. Metered + deadline-bounded on both paths.
pub(crate) struct BoxEgress {
    /// Operator-declared box-direct bindings: logical name → loopback endpoint URL.
    local: Arc<HashMap<String, String>>,
    /// Shared `reqwest` client for box-direct POSTs (reuses the process rustls/aws-lc-rs stack).
    client: Client,
    /// Runtime handle to drive the HTTP round-trip via `block_on` on the `spawn_blocking` thread.
    handle: Handle,
    /// Absolute per-execution deadline bounding every box-direct round-trip.
    deadline: Instant,
    /// The broker session for non-local names; `None` when the request named only box-direct
    /// resources (no broker session was opened).
    broker: Option<Arc<BrokerEgress>>,
    /// The request's **trusted** tenant id, forwarded to the box-direct loopback service as an
    /// out-of-band `X-Runlet-Tenant` header so a multitenant local service can scope by tenant —
    /// the box-direct analogue of the broker path's `WireInit.tenant`. Sourced only from the
    /// trusted-header extractor (never the script). `None` on the single-tenant/non-trusted path,
    /// where no header is emitted and the request is byte-for-byte unchanged.
    tenant: Option<String>,
    /// The request's **trusted** acting subject, forwarded to the box-direct loopback service as an
    /// out-of-band `X-Runlet-Actor` header so a consumer can build a who-did-what audit trail — the
    /// *who* companion to `tenant`'s *where*. The bare subject only. Sourced only from the
    /// trusted-header extractor (`TrustedIdentity.user`), never the script or request `payload` (an
    /// actor is a trust assertion, not a routing key). `None` on the single-tenant/non-trusted path,
    /// where no header is emitted.
    actor: Option<String>,
    /// Box-direct per-op metrics, keyed by logical name — drained into `meta.io.<name>`.
    metrics: Mutex<BTreeMap<String, Vec<LocalIoMetric>>>,
}

impl BoxEgress {
    /// Wraps the box-direct bindings + client and (optionally) the broker session. Build this on the
    /// `spawn_blocking` thread — its calls `block_on`.
    #[expect(
        clippy::too_many_arguments,
        reason = "one internal constructor called once per build site from execute_blocking; grouping \
                  these already-distinct per-request values into a params struct would be pure indirection"
    )]
    pub(crate) fn new(
        local: Arc<HashMap<String, String>>,
        client: Client,
        handle: Handle,
        budget: Duration,
        broker: Option<Arc<BrokerEgress>>,
        tenant: Option<String>,
        actor: Option<String>,
    ) -> Self {
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        Self {
            local,
            client,
            handle,
            deadline,
            broker,
            tenant,
            actor,
            metrics: Mutex::new(BTreeMap::new()),
        }
    }

    /// The box-direct per-op metrics recorded this session, keyed by logical name.
    pub(crate) fn drain_local(&self) -> BTreeMap<String, Vec<LocalIoMetric>> {
        self.handle.block_on(async {
            let mut guard = self.metrics.lock().await;
            mem::take(&mut *guard)
        })
    }

    /// Builds the box-direct POST for `url` with the serialized `body`, attaching the trusted tenant
    /// (`X-Runlet-Tenant`, *where*) and acting subject (`X-Runlet-Actor`, *who*) as out-of-band headers
    /// **only** when each is present. The `{action, payload}` body is unchanged either way, so a service
    /// moves between box-direct and broker with no wire-body change (D9).
    fn build_request(&self, url: &str, body: String) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json");
        if let Some(tenant) = self.tenant.as_deref() {
            request = request.header(TENANT_HEADER, tenant);
        }
        if let Some(actor) = self.actor.as_deref() {
            request = request.header(ACTOR_HEADER, actor);
        }
        request.body(body)
    }

    /// Performs one box-direct POST to `url`, records its metric under `name`, and maps the result
    /// to the FFI JSON (success body verbatim) or a `__runlet`-tagged [`EgressError`].
    fn call_local(
        &self,
        name: &str,
        url: &str,
        action: &str,
        payload: &str,
    ) -> Result<String, EgressError> {
        let body = serde_json::to_string(&LocalCallEnvelope { action, payload })
            .unwrap_or_else(|_err| String::from("{}"));
        let request_bytes = body.len();
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let start = Instant::now();
        let request = self.build_request(url, body);
        let outcome = self.handle.block_on(async {
            match timeout(remaining, request.send()).await {
                Ok(Ok(response)) => {
                    let status = response.status();
                    match response.text().await {
                        Ok(text) => Ok((status.as_u16(), status.is_success(), text)),
                        Err(err) => Err((0_u16, format!("box-direct read failed: {err}"))),
                    }
                }
                Ok(Err(err)) => Err((0_u16, format!("box-direct request failed: {err}"))),
                Err(_elapsed) => Err((
                    0_u16,
                    "box-direct call exceeded the execution deadline".to_owned(),
                )),
            }
        });
        let (status, result) = match outcome {
            Ok((code, true, text)) => (code, Ok(text)),
            Ok((code, false, _text)) => (
                code,
                Err(local_error(
                    name,
                    "IO_LOCAL_HTTP",
                    &format!("local endpoint returned HTTP {code}"),
                )),
            ),
            Err((code, message)) => (code, Err(local_error(name, "IO_TRANSPORT", &message))),
        };
        self.record(
            name,
            LocalIoMetric {
                action: action.to_owned(),
                duration_us: start.elapsed().as_micros(),
                request_bytes,
                status,
            },
        );
        result
    }

    /// Appends a box-direct op `metric` under `name`.
    fn record(&self, name: &str, metric: LocalIoMetric) {
        self.handle.block_on(async {
            let mut guard = self.metrics.lock().await;
            guard.entry(name.to_owned()).or_default().push(metric);
        });
    }
}

impl Egress for BoxEgress {
    fn call(&self, name: &str, action: &str, payload_json: &str) -> Result<String, EgressError> {
        // Resolution order (D8): a name declared box-direct resolves locally; otherwise it forwards
        // to the broker (the allowlist was already enforced by the mux before this point).
        if let Some(url) = self.local.get(name) {
            return self.call_local(name, url, action, payload_json);
        }
        self.broker.as_ref().map_or_else(
            || {
                Err(local_error(
                    name,
                    "EGRESS_UNAVAILABLE",
                    "no broker session for a non-local resource",
                ))
            },
            |broker| broker.call(name, action, payload_json),
        )
    }
}

impl MeteredEgress for BoxEgress {
    fn drain_metrics(&self) -> BackendMetrics {
        // Only the broker path produces `BackendMetrics` (per-kind); box-direct metrics are drained
        // separately via [`Self::drain_local`] and merged into `meta.io.<name>`.
        self.broker
            .as_ref()
            .map_or_else(BackendMetrics::default, |broker| broker.drain_metrics())
    }
}

/// Builds an [`EgressError`] tagged to the calling capability `name` so the engine classifies it as
/// a (retryable) capability error, exactly like an in-process backend fault.
fn local_error(name: &str, code: &str, message: &str) -> EgressError {
    EgressError::new(name, code, message.to_owned())
        .retryable()
        .owner(ErrorOwner::Operator)
}

#[cfg(test)]
mod tests {
    //! Box-direct identity propagation: the trusted tenant (`X-Runlet-Tenant`, *where*) and acting
    //! subject (`X-Runlet-Actor`, *who*) ride out-of-band headers while the `{action, payload}` body
    //! stays byte-identical (D9). These assert on the built request (no network), so no loopback
    //! server or `block_on` threading is involved.

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use reqwest::Client;
    use tokio::runtime::Handle;

    use super::{ACTOR_HEADER, BoxEgress, LocalCallEnvelope, TENANT_HEADER};

    /// A `BoxEgress` with one box-direct binding and the given trusted tenant + actor, on the test
    /// runtime's handle. The URL is never dialed — the tests only inspect the request the box builds.
    fn egress_with(tenant: Option<String>, actor: Option<String>) -> BoxEgress {
        let local: HashMap<String, String> =
            HashMap::from([("pricing".to_owned(), "http://127.0.0.1:9/".to_owned())]);
        BoxEgress::new(
            Arc::new(local),
            Client::new(),
            Handle::current(),
            Duration::from_secs(5),
            None,
            tenant,
            actor,
        )
    }

    /// The exact `{action, payload}` envelope the box serializes for a box-direct call.
    fn envelope(action: &str, payload: &str) -> String {
        serde_json::to_string(&LocalCallEnvelope { action, payload }).unwrap_or_default()
    }

    /// Reads a header from the built request as a `&str`.
    fn header<'a>(request: &'a reqwest::Request, name: &str) -> Option<&'a str> {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    /// The `{action, payload}` bytes the built request actually carries.
    fn body_bytes(request: &reqwest::Request) -> Vec<u8> {
        request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .unwrap_or_default()
            .to_vec()
    }

    #[tokio::test]
    async fn tenant_present_adds_header_and_leaves_body_identical() {
        let egress = egress_with(Some("ws_acme".to_owned()), None);
        let body = envelope("query", "{\"x\":1}");
        let request = egress
            .build_request("http://127.0.0.1:9/", body.clone())
            .build()
            .unwrap_or_else(|_err| unreachable!("a POST with a static body always builds"));

        // The trusted tenant rides the out-of-band header.
        assert_eq!(header(&request, TENANT_HEADER), Some("ws_acme"));

        // D9: the body is exactly the {action, payload} envelope — the tenant never leaks into it.
        assert_eq!(body_bytes(&request), body.as_bytes());
        assert!(!body.contains("tenant"));
    }

    #[tokio::test]
    async fn no_tenant_omits_header() {
        let egress = egress_with(None, None);
        let body = envelope("query", "{}");
        let request = egress
            .build_request("http://127.0.0.1:9/", body.clone())
            .build()
            .unwrap_or_else(|_err| unreachable!("a POST with a static body always builds"));

        // Single-tenant / non-trusted path: no header, and the body is unchanged.
        assert!(request.headers().get(TENANT_HEADER).is_none());
        assert_eq!(body_bytes(&request), body.as_bytes());
    }

    #[tokio::test]
    async fn actor_present_adds_header_and_leaves_body_identical() {
        let egress = egress_with(None, Some("u_42".to_owned()));
        let body = envelope("append", "{\"stream\":\"orders\"}");
        let request = egress
            .build_request("http://127.0.0.1:9/", body.clone())
            .build()
            .unwrap_or_else(|_err| unreachable!("a POST with a static body always builds"));

        // The trusted subject rides the out-of-band header; the body is untouched (D9).
        assert_eq!(header(&request, ACTOR_HEADER), Some("u_42"));
        assert_eq!(body_bytes(&request), body.as_bytes());
        assert!(!body.contains("actor"));
    }

    #[tokio::test]
    async fn no_actor_omits_header() {
        let egress = egress_with(Some("ws_acme".to_owned()), None);
        let body = envelope("query", "{}");
        let request = egress
            .build_request("http://127.0.0.1:9/", body.clone())
            .build()
            .unwrap_or_else(|_err| unreachable!("a POST with a static body always builds"));

        // A tenant without a subject emits no actor header.
        assert!(request.headers().get(ACTOR_HEADER).is_none());
    }

    #[tokio::test]
    async fn tenant_and_actor_ride_together() {
        let egress = egress_with(Some("ws_acme".to_owned()), Some("u_42".to_owned()));
        let body = envelope("append", "{\"x\":1}");
        let request = egress
            .build_request("http://127.0.0.1:9/", body.clone())
            .build()
            .unwrap_or_else(|_err| unreachable!("a POST with a static body always builds"));

        // Both identity dimensions ride out-of-band; the body is still exactly {action, payload}.
        assert_eq!(header(&request, TENANT_HEADER), Some("ws_acme"));
        assert_eq!(header(&request, ACTOR_HEADER), Some("u_42"));
        assert_eq!(body_bytes(&request), body.as_bytes());
    }
}
