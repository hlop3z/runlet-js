//! # Bring your own capability — a worked example
//!
//! A capability is how a sandboxed handler reaches the outside world. This example builds the
//! smallest possible one — an in-memory key/value store called `kv` — so you can fork it into a
//! real driver (a database, an HTTP API, a queue, …).
//!
//! Run it:
//!
//! ```text
//! cargo run -p kv-capability
//! ```
//!
//! It composes a [`LogicHost`] with the `kv` capability, runs one handler that does
//! `$std.kv.set('name', 'Ada')` then `$std.kv.get('name')`, and checks the value came back.
//!
//! A capability is **four pieces that must agree**:
//!
//! ```text
//!   handler (JS)          wrapper (JS)            the mux            backend (Rust)
//!  $std.kv.get(k) ──▶ io.call('kv','get',…) ──▶ routes 'kv' ──▶ KvBackend::call("get")
//!        ▲                    │                     │                    │
//!        └──── "Ada" ─────────┴───── {value:"Ada"} ─┴──── {"value":"Ada"}┘
//! ```
//!
//! The action token (`'get'` / `'set'`) is the same `snake_case` string in the JS wrapper and in
//! the backend's `match` — that agreement is the whole trick. See the fork-me guide in
//! `docs/03-capabilities.md`.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex, MutexGuard};

use runlet_core::modules::ModuleRegistry;
use runlet_core::pool::JsPool;
use runlet_core::registry::ScriptRegistry;
use runlet_core::{
    CapabilityDef, CapabilitySet, Egress, EgressError, EngineConfig, ExecOutcome, HostSettings,
    Invocation, LogicHost, Trust,
};
use serde_json::{Value, json};

// ── Piece 1: the JS wrapper ──────────────────────────────────────────────────────────────────
// Exposes `$std.kv` to the handler. Every method routes through the generic `io.call` mux by the
// capability name. `io.channel('kv')` binds the name once so we don't repeat it.
const KV_WRAPPER_JS: &str = r#"
(function () {
  var kv = $std.io.channel('kv');
  $std.kv = {
    // Read the value stored under `key` (null if it was never set).
    get: function (key) { return kv('get', { key: key }).value; },
    // Store `value` under `key`; returns { ok: true }.
    set: function (key, value) { return kv('set', { key: key, value: value }); }
  };
})();
"#;

// ── Piece 2: the editor types (.d.ts) ────────────────────────────────────────────────────────
// A real capability ships this fragment so a script author gets IntelliSense. It's the developer's
// own file — it is NOT folded into the box's shared `container/types.d.ts`.
const KV_TYPES_DTS: &str = r#"
/** The `kv` example capability, reachable in a handler as `$std.kv`. */
interface KvExampleCapability {
  /** Read the value stored under `key` (null if the key was never set). */
  get(key: string): string | null;
  /** Store `value` under `key`; returns `{ ok: true }`. */
  set(key: string, value: string): { ok: boolean };
}
"#;

// The handler we run below. Handlers return the response envelope via `json(data, error)`.
const HANDLER_JS: &str = r#"
function handler(ctx) {
  $std.kv.set('name', 'Ada');
  return json($std.kv.get('name'), null);
}
"#;

/// ── Piece 3: the backend ──
/// Answers the capability's calls. Here it's a plain in-memory map; swap this body for a real
/// driver (SQL, HTTP, …) and everything else stays the same.
#[derive(Default)]
struct KvBackend {
    store: Arc<Mutex<HashMap<String, String>>>,
}

impl KvBackend {
    /// Locks the map, recovering the guard if a previous holder panicked — a poisoned lock must
    /// not take the whole capability down.
    fn locked(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn handle_get(&self, payload: &Value) -> Result<String, EgressError> {
        let key = require_str(payload, "key")?;
        let value = self.locked().get(key).cloned();
        encode(&json!({ "value": value }))
    }

    fn handle_set(&self, payload: &Value) -> Result<String, EgressError> {
        let key = require_str(payload, "key")?;
        let value = require_str(payload, "value")?;
        self.locked().insert(key.to_owned(), value.to_owned());
        encode(&json!({ "ok": true }))
    }
}

impl Egress for KvBackend {
    /// The one method a capability backend implements: given the capability `name`, an `action`,
    /// and the JSON `payload` the wrapper built, do the work and return a JSON string.
    fn call(&self, _name: &str, action: &str, payload_json: &str) -> Result<String, EgressError> {
        let payload: Value = serde_json::from_str(payload_json)
            .map_err(|err| EgressError::new("kv", "KV_BAD_PAYLOAD", err.to_string()))?;
        match action {
            // These tokens MUST match the ones the JS wrapper passes to io.call.
            "get" => self.handle_get(&payload),
            "set" => self.handle_set(&payload),
            other => Err(EgressError::new(
                "kv",
                "KV_BAD_ACTION",
                format!("unknown kv action: {other}"),
            )),
        }
    }
}

/// Pulls a required string field out of the payload, or returns a capability error.
fn require_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str, EgressError> {
    payload.get(field).and_then(Value::as_str).ok_or_else(|| {
        EgressError::new(
            "kv",
            "KV_BAD_PAYLOAD",
            format!("missing string field `{field}`"),
        )
    })
}

/// Serializes a reply value to the JSON string the mux expects.
fn encode(value: &Value) -> Result<String, EgressError> {
    serde_json::to_string(value).map_err(|err| EgressError::new("kv", "KV_ENCODE", err.to_string()))
}

/// ── Piece 4: wiring ──
/// Compose a host with the `kv` capability. `Trust::OperatorSupplied` because `kv` has no
/// script-chosen network target — nothing to SSRF-guard. A capability with a bound backend serves
/// its own calls in-process; no sidecar needed.
fn build_host() -> Result<LogicHost, Box<dyn Error + Send + Sync>> {
    let config = EngineConfig::default();
    let pool = JsPool::new(config, Arc::new(ModuleRegistry::default()))?;
    let registry = Arc::new(ScriptRegistry::default());
    let settings = HostSettings {
        limits: config,
        allow_private_targets: false,
    };

    let kv = CapabilityDef::new("kv", KV_WRAPPER_JS, KV_TYPES_DTS, Trust::OperatorSupplied)
        .with_backend(Arc::new(KvBackend::default()));

    LogicHost::builder(pool, registry, settings)
        .capability(kv)
        .build()
        .map_err(|err| format!("failed to build host: {err}").into())
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let host = build_host()?;

    // `caps.io` names which capabilities this request may use — the per-request opt-in gate.
    let caps = CapabilitySet {
        io: &["kv"],
        ..CapabilitySet::NONE
    };
    let invocation = Invocation::inline(HANDLER_JS, "{}").caps(caps);

    let outcome = host
        .run(invocation)
        .map_err(|err| format!("execution failed: {err:?}"))?;

    match outcome.result {
        ExecOutcome::Success(envelope) => {
            println!("handler returned: {envelope}");
            let parsed: Value = serde_json::from_str(&envelope)?;
            let data = parsed
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if data != "Ada" {
                return Err(format!("round-trip failed: expected \"Ada\", got {data:?}").into());
            }
            println!("kv round-trip OK: $std.kv.get('name') == {data:?}");
            Ok(())
        }
        ExecOutcome::Error(err) => Err(format!("handler error: {err:?}").into()),
    }
}
