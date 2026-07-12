use super::{EngineError, ExecOutcome, ExecParams, Profile, run};
use crate::capability::{
    CapabilityDef, CapabilityRegistry, SsrfPolicy, Target, TargetExtractor, Trust,
};
use crate::egress::{Egress, EgressError};
use rquickjs::Runtime;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A stub egress: action `"fail"` returns a retryable `db` error; anything else echoes the
/// payload back wrapped in `{"echoed": …}` (valid JSON, since the wrapper stringified it).
struct EchoEgress;

impl Egress for EchoEgress {
    fn call(&self, _name: &str, action: &str, payload_json: &str) -> Result<String, EgressError> {
        if action == "fail" {
            return Err(EgressError::new("db", "DB_TIMEOUT", "backend unreachable").retryable());
        }
        Ok(format!("{{\"echoed\":{payload_json}}}"))
    }
}

/// An egress that flips a shared flag when reached and replies with a fixed marker — proves
/// whether the mux dispatched to the backend or blocked before it.
struct RecordingEgress {
    /// Set to `true` the first time [`Egress::call`] runs.
    called: Arc<AtomicBool>,
    /// Fixed JSON reply on success.
    reply: String,
}

impl Egress for RecordingEgress {
    fn call(&self, _name: &str, _action: &str, _payload_json: &str) -> Result<String, EgressError> {
        self.called.store(true, Ordering::Relaxed);
        Ok(self.reply.clone())
    }
}

/// A `ScriptControlled` policy whose extractor reads `payload.host` (port 80), with an empty
/// allowlist so only the private-IP block decides.
fn host_policy() -> SsrfPolicy {
    let extractor: Arc<TargetExtractor> = Arc::new(|payload: &str| {
        let value: serde_json::Value =
            serde_json::from_str(payload).map_err(|err| err.to_string())?;
        let host = value
            .get("host")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing host".to_owned())?;
        Ok(Some(Target {
            host: host.to_owned(),
            port: 80,
        }))
    });
    SsrfPolicy::new(Arc::from(Vec::<String>::new()), extractor)
}

/// The registry wiring for a test run: an optional registry plus the enabled capability names
/// (bundled to keep [`params`] within the argument-count lint).
#[derive(Clone, Copy)]
struct Reg<'a> {
    /// The host's composed registry, or `None` for a fallback-only run.
    registry: Option<&'a CapabilityRegistry>,
    /// Registered names whose wrappers to inject for this request.
    enabled: &'a [&'a str],
}

impl<'a> Reg<'a> {
    /// No registry and no enabled names (a raw `io.call` / fallback-only run).
    const NONE: Reg<'static> = Reg {
        registry: None,
        enabled: &[],
    };

    /// A registry with the given enabled names.
    const fn new(registry: &'a CapabilityRegistry, enabled: &'a [&'a str]) -> Self {
        Self {
            registry: Some(registry),
            enabled,
        }
    }

    /// No registry, but the given names allowlisted (a fallback-only run whose `io.call` names
    /// must still pass the D3 allowlist gate).
    const fn names(enabled: &'a [&'a str]) -> Self {
        Self {
            registry: None,
            enabled,
        }
    }
}

/// Builds `ExecParams` for the no-capability build with a registry wiring and an optional
/// per-request fallback egress.
fn params<'a>(
    runtime: &'a Runtime,
    script: &'a str,
    profile: Profile,
    reg: Reg<'a>,
    egress: Option<Arc<dyn Egress>>,
) -> ExecParams<'a> {
    ExecParams {
        runtime,
        bytecode_cache: None,
        cache_namespace: None,
        script,
        context_json: "{\"n\":7}",
        timeout: Duration::from_secs(5),
        profile,
        sys_config: None,
        registry: reg.registry,
        enabled_io: reg.enabled,
        egress,
        default_currency: None,
        max_ops: 8,
        max_emit_kind_len: 64,
        log_level: super::LogLevel::Info,
        max_log_entries: 256,
        max_log_entry_bytes: 256 * 1024,
        max_log_total_bytes: 1024 * 1024,
        max_output_size: 0,
        allow_private_targets: false,
    }
}

/// Runs `script` and returns the success JSON, failing the test on a non-success outcome.
fn run_ok(params: &ExecParams<'_>) -> String {
    let exec = run(params).unwrap_or_else(|_err| unreachable!());
    let ExecOutcome::Success(json) = exec.outcome else {
        unreachable!("expected a success outcome");
    };
    json
}

/// A successful `io.call` (served by the per-request fallback egress) returns the backend
/// JSON to the script — an unregistered name routes to the fallback.
#[test]
fn resource_call_returns_backend_json() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let script =
        "function handler(ctx) { return json($std.io.call('orders', 'ping', { x: ctx.n })); }";
    let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::names(&["orders"]),
        Some(egress),
    ));
    assert!(
        json.contains("echoed"),
        "backend JSON flows to the script: {json}"
    );
    assert!(
        json.contains("\"x\":7"),
        "the payload round-tripped: {json}"
    );
}

/// A `EgressError` round-trips through the `__runlet` tag and classifies as a capability
/// error (not a generic script error), preserving the `db` source.
#[test]
fn resource_error_classifies_as_capability() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let script = "function handler(ctx) { return json($std.io.call('orders', 'fail', {})); }";
    let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
    let exec = run(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::names(&["orders"]),
        Some(egress),
    ))
    .unwrap_or_else(|_err| unreachable!());
    assert!(
        matches!(exec.outcome, ExecOutcome::Error(EngineError::Capability(_))),
        "a EgressError must surface as a classified capability error"
    );
}

/// Under `Profile::Deterministic` the mux is withheld: the `io` global is undefined even
/// when an egress is wired (the boundary is enforced by the engine, not the caller).
#[test]
fn resource_withheld_under_deterministic_profile() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let script = "function handler() { return json(typeof $std.io); }";
    let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Deterministic,
        Reg::NONE,
        Some(egress),
    ));
    assert!(
        json.contains("undefined"),
        "io withheld under deterministic: {json}"
    );
}

/// D9/1.7: under the deterministic profile the ambient nondeterministic authorities are
/// *removed* (`typeof === "undefined"`), not stubbed — a stub would report `"function"`, and
/// a present-but-gated authority is one refactor from being un-gated.
#[test]
fn deterministic_profile_removes_ambient_authority() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let script = "function handler() { return json({ \
            random: typeof Math.random, \
            dateNow: typeof Date.now }); }";
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Deterministic,
        Reg::NONE,
        None,
    ));
    assert!(
        json.contains("\"random\":\"undefined\""),
        "Math.random removed: {json}"
    );
    assert!(
        json.contains("\"dateNow\":\"undefined\""),
        "Date.now removed: {json}"
    );
}

/// A registered def's wrapper is injected only when its name is enabled; an unregistered /
/// disabled name has no global (`typeof x === "undefined"`).
#[test]
fn registered_wrapper_injected_only_when_enabled() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let wrapper =
        "globalThis.widget = { ping: function () { return $std.io.call('widget', 'ping', {}); } };";
    let def = CapabilityDef::new("widget", wrapper, "", Trust::OperatorSupplied);
    let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
    let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
    let script =
        "function handler() { return json({ widget: typeof widget, missing: typeof gadget }); }";
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["widget"]),
        Some(egress),
    ));
    assert!(
        json.contains("\"widget\":\"object\""),
        "enabled wrapper is present: {json}"
    );
    assert!(
        json.contains("\"missing\":\"undefined\""),
        "unregistered name is absent: {json}"
    );
}

/// D3 allowlist gate: an `io.call` to a name absent from the request allowlist is rejected
/// with `RESOURCE_NOT_FOUND` before the fallback backend is ever reached.
#[test]
fn unlisted_name_is_rejected_before_egress() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let called = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn Egress> = Arc::new(RecordingEgress {
        called: Arc::clone(&called),
        reply: "{}".to_owned(),
    });
    let script = "function handler() { \
            try { $std.io.call('secret', 'query', {}); return json('no throw'); } \
            catch (e) { return json(e.__runlet ? e.__runlet.code : 'untagged'); } }";
    // Allowlist enables only `orders`; the script asks for `secret`.
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::names(&["orders"]),
        Some(backend),
    ));
    assert!(
        json.contains("RESOURCE_NOT_FOUND"),
        "an unlisted name is rejected: {json}"
    );
    assert!(
        !called.load(Ordering::Relaxed),
        "the fallback backend must never be reached for an unlisted name"
    );
}

/// D1: two defs sharing a name are rejected at build time, before any request.
#[test]
fn duplicate_registration_is_rejected() {
    let one = CapabilityDef::new("db", "", "", Trust::OperatorSupplied);
    let two = CapabilityDef::new("db", "", "", Trust::OperatorSupplied);
    assert!(
        CapabilityRegistry::build(vec![one, two], None).is_err(),
        "duplicate capability names must fail host construction"
    );
}

/// D4/1.5: a `ScriptControlled` def targeting a private IP is rejected *before* the backend
/// is reached — the SSRF guard runs pre-connect, inside the mux.
#[test]
fn script_controlled_private_ip_rejected_pre_connect() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let called = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn Egress> = Arc::new(RecordingEgress {
        called: Arc::clone(&called),
        reply: "{\"ok\":true}".to_owned(),
    });
    let def = CapabilityDef::new("db", "", "", Trust::ScriptControlled(host_policy()))
        .with_backend(backend);
    let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
    let script =
        "function handler() { return json($std.io.call('db', 'get', { host: '10.0.0.1' })); }";
    let exec = run(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["db"]),
        None,
    ))
    .unwrap_or_else(|_err| unreachable!());
    assert!(
        matches!(exec.outcome, ExecOutcome::Error(_)),
        "a private-IP target must be rejected"
    );
    assert!(
        !called.load(Ordering::Relaxed),
        "the backend must never be reached (pre-connect block)"
    );
}

/// A `ScriptControlled` def targeting a public host is permitted through to the backend.
#[test]
fn script_controlled_public_host_reaches_backend() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let called = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn Egress> = Arc::new(RecordingEgress {
        called: Arc::clone(&called),
        reply: "{\"ok\":true}".to_owned(),
    });
    let def = CapabilityDef::new("db", "", "", Trust::ScriptControlled(host_policy()))
        .with_backend(backend);
    let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
    let script =
        "function handler() { return json($std.io.call('db', 'get', { host: '93.184.216.34' })); }";
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["db"]),
        None,
    ));
    assert!(
        json.contains("\"ok\":true"),
        "public host reaches the backend: {json}"
    );
    assert!(called.load(Ordering::Relaxed), "the backend was reached");
}

/// D9/1.6: when the trust-policy hook itself errors, the mux denies the call rather than
/// falling through to the I/O.
#[test]
fn mux_fails_closed_on_policy_error() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let called = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn Egress> = Arc::new(RecordingEgress {
        called: Arc::clone(&called),
        reply: "{}".to_owned(),
    });
    // A policy whose extractor always fails — the enforcement cannot be evaluated.
    let extractor: Arc<TargetExtractor> =
        Arc::new(|_payload: &str| Err("policy hook exploded".to_owned()));
    let policy = SsrfPolicy::new(Arc::from(Vec::<String>::new()), extractor);
    let def =
        CapabilityDef::new("db", "", "", Trust::ScriptControlled(policy)).with_backend(backend);
    let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
    let script = "function handler() { return json($std.io.call('db', 'get', {})); }";
    let exec = run(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["db"]),
        None,
    ))
    .unwrap_or_else(|_err| unreachable!());
    assert!(
        matches!(exec.outcome, ExecOutcome::Error(_)),
        "a failing policy hook must deny the call"
    );
    assert!(
        !called.load(Ordering::Relaxed),
        "the backend must never be reached when enforcement fails"
    );
}

/// D9: a *panicking* enforcement hook also denies (fail-closed), never reaching the backend.
#[test]
fn mux_fails_closed_on_policy_panic() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let called = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn Egress> = Arc::new(RecordingEgress {
        called: Arc::clone(&called),
        reply: "{}".to_owned(),
    });
    let extractor: Arc<TargetExtractor> =
        Arc::new(|_payload: &str| unreachable!("policy hook panics"));
    let policy = SsrfPolicy::new(Arc::from(Vec::<String>::new()), extractor);
    let def =
        CapabilityDef::new("db", "", "", Trust::ScriptControlled(policy)).with_backend(backend);
    let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
    let script = "function handler() { return json($std.io.call('db', 'get', {})); }";
    let exec = run(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["db"]),
        None,
    ))
    .unwrap_or_else(|_err| unreachable!());
    assert!(
        matches!(exec.outcome, ExecOutcome::Error(_)),
        "a panicking policy hook must deny the call"
    );
    assert!(
        !called.load(Ordering::Relaxed),
        "the backend must never be reached on a panic"
    );
}

/// D5/1.8: mixed topology — one name served by a local backend, another falling through to
/// the per-request fallback egress, in the same execution.
#[test]
fn mixed_topology_local_and_fallback() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let local_called = Arc::new(AtomicBool::new(false));
    let local: Arc<dyn Egress> = Arc::new(RecordingEgress {
        called: Arc::clone(&local_called),
        reply: "{\"served\":\"local\"}".to_owned(),
    });
    // `orders` is bound locally; `amq` is registered without a backend → the fallback serves it.
    let orders = CapabilityDef::new("orders", "", "", Trust::OperatorSupplied).with_backend(local);
    let amq = CapabilityDef::new("amq", "", "", Trust::OperatorSupplied);
    let reg =
        CapabilityRegistry::build(vec![orders, amq], None).unwrap_or_else(|_err| unreachable!());
    let fallback: Arc<dyn Egress> = Arc::new(EchoEgress);
    let script = "function handler() { return json({ \
            a: $std.io.call('orders', 'ping', {}), \
            b: $std.io.call('amq', 'publish', { m: 1 }) }); }";
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["orders", "amq"]),
        Some(fallback),
    ));
    assert!(
        json.contains("\"served\":\"local\""),
        "orders served in-process: {json}"
    );
    assert!(
        json.contains("echoed"),
        "amq served by the fallback: {json}"
    );
    assert!(
        local_called.load(Ordering::Relaxed),
        "the local backend was used"
    );
}

/// A registered name with neither a local backend nor a fallback fails with
/// `EGRESS_UNAVAILABLE`.
#[test]
fn no_backend_no_fallback_is_egress_unavailable() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let def = CapabilityDef::new("db", "", "", Trust::OperatorSupplied);
    let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
    let script = "function handler() { \
            try { $std.io.call('db', 'query', {}); return json('no throw'); } \
            catch (e) { return json(e.__runlet ? e.__runlet.code : 'untagged'); } }";
    let json = run_ok(&params(
        &runtime,
        script,
        Profile::Full,
        Reg::new(&reg, &["db"]),
        None,
    ));
    assert!(
        json.contains("EGRESS_UNAVAILABLE"),
        "no backend + no fallback: {json}"
    );
}
