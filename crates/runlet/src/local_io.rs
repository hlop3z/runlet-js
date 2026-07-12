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
    /// Box-direct per-op metrics, keyed by logical name — drained into `meta.io.<name>`.
    metrics: Mutex<BTreeMap<String, Vec<LocalIoMetric>>>,
}

impl BoxEgress {
    /// Wraps the box-direct bindings + client and (optionally) the broker session. Build this on the
    /// `spawn_blocking` thread — its calls `block_on`.
    pub(crate) fn new(
        local: Arc<HashMap<String, String>>,
        client: Client,
        handle: Handle,
        budget: Duration,
        broker: Option<Arc<BrokerEgress>>,
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
        let outcome = self.handle.block_on(async {
            let request = self
                .client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .body(body);
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
