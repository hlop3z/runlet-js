//! `QuickJS` execution engine — hardened sandbox for `handler(context)`.
//!
//! Uses `ctx.json_parse()` / `Function::call()` for direct C FFI data exchange.
//!
//! Sandbox: memory + stack limits, execution timeout, `eval()`/`Proxy` removed,
//! fresh context per request.
//!
//! On failure the engine **classifies** the outcome into a typed [`EngineError`]
//! (see `docs/99-errors.md`): a handler throw is inspected *structurally* via
//! `ctx.catch()` — a `__runlet` tag ⇒ a capability error, otherwise a script error —
//! and the timeout signal (which JS cannot see) is folded in here. Out-of-memory is
//! caught earlier, when an oversized context fails to parse.

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::module::{Declared, Evaluated};
use rquickjs::{Context, Ctx, Function, Module, Object, Runtime, Value as JsValue, WriteOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::bytecode::{self, BytecodeCache};
use crate::capability::{CapabilityRegistry, MuxCall};
use crate::decimal;
use crate::egress::Egress;
use crate::errors::{self, ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
#[cfg(feature = "http")]
use crate::http::{self, HttpMetric};
use crate::modules;
use crate::money;
#[cfg(feature = "s3")]
use crate::s3::{self, S3Config, S3Metric};
// The metric collector apparatus is needed only by the in-engine capabilities (`http`/`s3`); the
// driver-backed capabilities surface their metrics from the egress adapter, not the engine.
#[cfg(any(feature = "http", feature = "s3"))]
use crate::sandbox::{self, Collector};
use crate::sys::{self, SysConfig};

/// The `json()` bridge — loaded from `src/js/bridge.js` at compile time.
const JSON_BRIDGE: &str = include_str!("js/bridge.js");

/// Shared FFI primitives (`__ffi.unwrap`, the `__runlet` tagged-error contract) — loaded from
/// `src/js/ffi.js` at compile time. Injected unconditionally with the bridge so it is present for
/// both egress surfaces (`io.js` and the `s3` bypass), which are gated independently.
const FFI_PRIMITIVES: &str = include_str!("js/ffi.js");

/// Human-safe message for a missing `handler`.
const HANDLER_MISSING_MSG: &str = "script must define a `handler(context)` function";
/// Human-safe message for an out-of-memory abort.
const MEMORY_MSG: &str = "memory limit exceeded";

/// Determinism sanitizer — loaded from `src/js/determinism.js` at compile time. Run after
/// `sanitize_globals` under [`Profile::Deterministic`] to neutralize nondeterministic
/// surfaces (`Math.random`, `Date.now`, zero-arg `new Date()`, `$sys.date.now`,
/// `$sys.crypto.uuid`).
const DETERMINISM_SANITIZER: &str = include_str!("js/determinism.js");

/// The generic `io.call` egress wrapper — loaded from `src/js/io.js` at compile
/// time. `eval`'d after `__io` is registered, only when a [`Egress`] is wired and the
/// profile is `Full` (the seam is I/O).
const IO_WRAPPER: &str = include_str!("js/io.js");

/// The `log.*` diagnostic wrapper — loaded from `src/js/log.js` at compile time. `eval`'d after
/// `__log` + `__logFloor` are registered; injected under both profiles (D8: a deterministic script
/// is often exactly the one you want to debug), so it sits beside `inject_emit` in `run`.
const LOG_WRAPPER: &str = include_str!("js/log.js");

/// Capability-injection + determinism profile for an execution.
///
/// A **runtime** injection decision (not a compile-time feature) so a single process can
/// run both tiers — see the consuming spec's "logic plane".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The full jsbox capability set (per-request, opt-in) plus `emit`. The post-commit /
    /// action tier — essentially jsbox's existing behavior.
    Full,
    /// No I/O capabilities are injected (`db`/`http`/`mongo`/`mail`/`s3`/`redis`/`amq`/
    /// `auth` are all withheld) and nondeterminism is neutralized on top of the existing
    /// `eval`/`Proxy` removal. Only the pure `$`/`$sys` helpers, `emit`, and a
    /// consumer-supplied read-of-declared-dependencies hook are available. The
    /// in-transaction logic tier.
    Deterministic,
}

/// Whether a driver-backed capability's wrapper is injected for an invocation.
///
/// A two-variant enum rather than a `bool` so the capability structs don't accumulate a wall of
/// bools (`clippy::struct_excessive_bools`). Encodes presence only — the connection and
/// credentials live in the wired [`Egress`] port, resolved operator-side from a logical resource
/// name, never crossing the engine boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Gate {
    /// Capability withheld (the default).
    #[default]
    Off,
    /// Capability exposed — its JS wrapper is injected.
    On,
}

impl Gate {
    /// `On` when `enabled`, else `Off`.
    #[must_use]
    pub const fn from_enabled(enabled: bool) -> Self {
        if enabled { Self::On } else { Self::Off }
    }

    /// Whether the capability is exposed.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// The per-invocation `emit` buffer: native `__emit` appends `(kind, value_json)` pairs, drained
/// into [`ExecResult::effects`] after execution.
type EmitBuffer = Arc<Mutex<Vec<(String, String)>>>;

/// A single declarative effect: a required `kind` routing tag plus an opaque `value`.
///
/// Produced by `emit(kind, value)` (verbatim `JSON.stringify` output) and surfaced in call order
/// on [`ExecResult::effects`] / [`crate::host::Outcome::effects`]. The core surfaces `kind`
/// structurally but never interprets it (the routing/governance seam); `value` stays fully opaque.
#[derive(Debug, Serialize)]
pub struct Effect {
    /// The required non-empty routing tag (bounded by `max_emit_kind_len`).
    pub kind: String,
    /// The opaque emitted value, preserved verbatim from `JSON.stringify`.
    pub value: Box<RawValue>,
}

/// Diagnostic log severity for the `log.*` channel.
///
/// Ordered `Trace < Debug < Info < Warn < Error` (the D6 level floor; the derived [`Ord`] follows
/// declaration order). A call below the configured floor is discarded in the JS wrapper before any
/// serialization (near-free on the hot path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// The most verbose level — fine-grained tracing.
    Trace,
    /// Debugging detail, off in production by default.
    Debug,
    /// The default floor: normal operational events.
    Info,
    /// A warning that did not stop the run.
    Warn,
    /// An error condition.
    Error,
}

impl LogLevel {
    /// The lowercase wire name (`"trace"`/`"debug"`/…), matching the `serde` representation and the
    /// names the JS wrapper passes across the FFI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// The numeric rank (`0`..=`4`) used as the JS-side level floor, so the wrapper can compare
    /// cheaply before building an entry.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Trace => 0,
            Self::Debug => 1,
            Self::Info => 2,
            Self::Warn => 3,
            Self::Error => 4,
        }
    }

    /// Parses a lowercase level name; `None` for an unknown token.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _unknown => None,
        }
    }
}

/// A single structured diagnostic entry produced by a `log.<level>(...)` call.
///
/// Carries the Serilog-style `template`, the merged `properties` (bound context + per-call fields,
/// opaque JSON to the core), the rendered `message`, a deterministic `seq` (call order), and — only
/// under [`Profile::Full`] — a relative `offset_us` from execution start (D8: no wall-clock timing
/// under the deterministic profile). An oversize entry is truncated with a `{"truncated":true}`
/// property marker (D7). The core defines this structure but never interprets a log's meaning.
#[derive(Debug, Serialize)]
pub struct LogEntry {
    /// Severity level.
    pub level: LogLevel,
    /// The message template (`"charged {user} {amount}"`).
    pub template: String,
    /// The merged named properties (opaque JSON object), verbatim from `JSON.stringify`.
    pub properties: Box<RawValue>,
    /// The rendered message (template with `{name}` placeholders substituted, rendered in JS).
    pub message: String,
    /// Monotonic per-execution sequence number preserving call order (starts at `0`).
    pub seq: u64,
    /// Relative microseconds from execution start; attached only under [`Profile::Full`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_us: Option<u128>,
}

/// One captured `(level, template, properties_json, message, seq, offset_us)` tuple, before it is
/// drained into a public [`LogEntry`]. Held in the [`LogBuffer`]'s accumulator.
struct RawLog {
    /// Severity level.
    level: LogLevel,
    /// The message template.
    template: String,
    /// The merged properties as a JSON string (verbatim from the wrapper's `JSON.stringify`).
    properties_json: String,
    /// The JS-rendered message.
    message: String,
    /// Call-order sequence number.
    seq: u64,
    /// Relative microseconds from execution start (`None` under the deterministic profile).
    offset_us: Option<u128>,
}

/// The per-invocation `log` accumulator: the captured entries plus a running byte total, so the D7
/// per-execution total bound (`max_log_total_bytes`) can be enforced across entries.
#[derive(Default)]
struct LogAccumulator {
    /// Captured entries, in call order.
    entries: Vec<RawLog>,
    /// Running sum of the captured entries' sizes (template + properties + message bytes).
    total_bytes: usize,
}

/// The shared per-invocation `log` buffer: native `__log` appends [`RawLog`]s here (bounded by the
/// D7 triad), drained into [`ExecResult::logs`] after execution on both paths (capture-on-failure).
type LogBuffer = Arc<Mutex<LogAccumulator>>;

/// One candidate log entry crossing from the native `__log` into [`capture_log`], before the D7
/// bounds decide to keep, truncate, or drop it. Bundled so the capture helper stays within the
/// argument-count lint.
struct PendingLog {
    /// Severity level.
    level: LogLevel,
    /// The message template.
    template: String,
    /// The merged properties as a JSON string.
    properties_json: String,
    /// The JS-rendered message.
    message: String,
    /// Relative microseconds from execution start (`None` under the deterministic profile).
    offset_us: Option<u128>,
}

/// The D7 per-execution log bounds + the resolved level floor, threaded into the native `__log`.
#[derive(Clone, Copy)]
struct LogLimits {
    /// The resolved minimum level (below it, the wrapper records nothing).
    floor: LogLevel,
    /// Max entries per execution; beyond it further calls are dropped.
    max_entries: usize,
    /// Max bytes per entry; an oversize entry is truncated with a marker.
    max_entry_bytes: usize,
    /// Max total bytes per execution (binds first); a call that would exceed it is dropped.
    max_total_bytes: usize,
}

/// Parameters for a single script execution. Built by the [`crate::host::LogicHost`] from
/// a public `Invocation`; internal to the core.
pub(crate) struct ExecParams<'a> {
    /// The pooled runtime.
    pub(crate) runtime: &'a Runtime,
    /// Shared compiled-bytecode cache (parse/compile reuse for the ES-module path).
    /// `None` = always recompile (e.g. a consumer that opts out).
    pub(crate) bytecode_cache: Option<&'a BytecodeCache>,
    /// Partition/tenant namespace mixed into the bytecode cache key, so identical source from
    /// different tenants does not share an entry. `None` = global (no namespace).
    pub(crate) cache_namespace: Option<&'a str>,
    /// JS script source.
    pub(crate) script: &'a str,
    /// Context JSON string.
    pub(crate) context_json: &'a str,
    /// Execution timeout.
    pub(crate) timeout: Duration,
    /// Capability + determinism profile (gates I/O injection and the determinism sanitizer).
    pub(crate) profile: Profile,
    /// Allowed HTTP hosts (empty = disabled).
    #[cfg(feature = "http")]
    pub(crate) allowed_hosts: &'a [String],
    /// S3 config (None = disabled). Stays in-engine (pure `SigV4` presign, no driver), so unlike
    /// the driver-backed capabilities it still carries its config across the boundary.
    #[cfg(feature = "s3")]
    pub(crate) s3_config: Option<&'a S3Config>,
    /// `$sys` env/secrets context (None = no env/secrets injected).
    pub(crate) sys_config: Option<&'a SysConfig>,
    /// The composed capability registry (the mux's per-name routing table + the JS wrappers to
    /// inject). `None` = no registered capabilities. Under [`Profile::Deterministic`] the mux and
    /// every wrapper are withheld (they perform I/O), regardless of registration.
    pub(crate) registry: Option<&'a CapabilityRegistry>,
    /// Names of registered egress capabilities to enable for this request (per-request, opt-in).
    /// A registered def's wrapper is injected only if its name appears here.
    pub(crate) enabled_io: &'a [&'a str],
    /// Per-request fallback egress for the mux (the `fabricd` sidecar). Consulted for any name
    /// without a local backend. `None` = no per-request fallback. Withheld under
    /// [`Profile::Deterministic`] (it performs I/O).
    pub(crate) egress: Option<Arc<dyn Egress>>,
    /// The resolved default currency for `$` / `money` construction (`config.currency` else the
    /// operator `default_currency`). `None` leaves the cascade fallback unset, so a currency-less
    /// construction throws.
    pub(crate) default_currency: Option<&'a str>,
    /// Max operations per execution (also caps the number of `emit` effects).
    pub(crate) max_ops: usize,
    /// Max character length of an `emit` `kind` tag; a longer tag is rejected.
    pub(crate) max_emit_kind_len: usize,
    /// The resolved diagnostic-log level floor (D6): a `log.<level>` call below this records
    /// nothing. Resolved by the host from `EngineConfig.log_level` and any trusted per-request
    /// override.
    pub(crate) log_level: LogLevel,
    /// Max diagnostic-log entries captured per execution (D7 count cap).
    pub(crate) max_log_entries: usize,
    /// Max bytes of a single diagnostic-log entry (D7 per-entry cap); an oversize entry is
    /// truncated with a marker.
    pub(crate) max_log_entry_bytes: usize,
    /// Max total diagnostic-log bytes per execution (D7 total cap — binds first).
    pub(crate) max_log_total_bytes: usize,
    /// Max bytes the handler may return (`0` = off, bounded only by `memory_limit`).
    pub(crate) max_output_size: usize,
    /// Debug mode: relax the SSRF private-IP block for local testing — the in-engine `http`/`s3`
    /// targets and the capability mux's `ScriptControlled` guard.
    pub(crate) allow_private_targets: bool,
    /// Whether the `http` client honors an `allowed_hosts: ["*"]` wildcard. Resolved in the
    /// handler as `allow_wildcard_hosts && !debug` — a wildcard is never honored in the
    /// SSRF-relaxed debug mode.
    #[cfg(feature = "http")]
    pub(crate) wildcard_hosts_allowed: bool,
}

/// Result of a script execution: the outcome, the declarative `emit` effects, and the
/// drained per-capability metrics. Internal to the core; the host maps it to a public
/// `Outcome`.
pub(crate) struct ExecResult {
    /// Success envelope or a classified error.
    pub(crate) outcome: ExecOutcome,
    /// Declarative effects appended via `emit(kind, value)`, in call order. Each carries a
    /// routing `kind` tag and an opaque `value` (the consumer interprets the value).
    pub(crate) effects: Vec<Effect>,
    /// Diagnostic log entries appended via `log.<level>(...)`, in call order, drained on both the
    /// success and error paths (capture-on-failure, D8). Outside the reproducible `data`/`effects`
    /// contract.
    pub(crate) logs: Vec<LogEntry>,
    /// HTTP requests made during execution (in-engine capability).
    #[cfg(feature = "http")]
    pub(crate) http_metrics: Vec<HttpMetric>,
    /// S3 presign operations made during execution (in-engine capability).
    #[cfg(feature = "s3")]
    pub(crate) s3_metrics: Vec<S3Metric>,
    // The driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) no longer report
    // metrics through the engine: they run in the wired egress adapter, which the consumer drains
    // directly (see `fabric_backends::BackendSet`). So the engine carries only the in-engine
    // capabilities' metrics here.
}

/// What the handler produced: a success envelope or a system error.
#[derive(Debug)]
pub enum ExecOutcome {
    /// Handler returned — the JS-produced `{"data": ..., "error": ...}` string.
    Success(String),
    /// A classified system error (runtime / script / capability).
    Error(EngineError),
}

/// A classified engine-level error, ready for the handler to assemble into a response.
#[derive(Debug)]
pub enum EngineError {
    /// `eval` of the script failed to parse.
    Syntax(String),
    /// A `CodeRef::Registered` key resolved to no script in the registry. (The HTTP front
    /// resolves keys itself and never produces this; it is for non-HTTP consumers that pass
    /// a key straight to the host.)
    ScriptNotFound(String),
    /// An ES-module handler `import`ed a specifier that isn't a registered module.
    ModuleNotFound(String),
    /// Script defines no `handler(context)`.
    HandlerNotDefined,
    /// Wall-clock limit hit (detected via the interrupt flag).
    Timeout {
        /// Configured limit, for the message.
        limit_ms: u128,
    },
    /// Memory cap exceeded (best-effort: thrown error named `InternalError`).
    MemoryLimit,
    /// `handler` returned something that isn't a `{data,error}` envelope.
    Malformed(String),
    /// The handler's returned JSON exceeded `max_output_size`.
    OutputTooLarge {
        /// Actual size produced.
        size: usize,
        /// Configured ceiling.
        limit: usize,
    },
    /// Our fault: context creation, capability injection, or a task panic.
    Internal(String),
    /// The host is shutting down and no longer accepts new executions (see
    /// [`crate::host::LogicHost::shutdown`]). Retryable — typically against another replica.
    ShuttingDown,
    /// Uncaught `throw` from the handler (an explicit `throw` or a script bug).
    Script {
        /// JS error message.
        message: String,
        /// JS stack trace, when available.
        stack: Option<String>,
    },
    /// A capability's native call failed and its wrapper threw a tagged error. Boxed: it
    /// is by far the largest variant (it carries `details`/`raw`/`stack`), so boxing keeps
    /// `EngineError` small in the common (non-capability) paths.
    Capability(Box<CapabilityErr>),
}

/// A capability error read off a thrown JS error's `__runlet` tag (or built for an
/// inject-time connection failure). Fields are private — only `into_envelope` reads them.
#[derive(Debug)]
pub struct CapabilityErr {
    /// Originating capability.
    source: ErrorSource,
    /// Stable machine code (set in Rust, round-tripped through the tag).
    code: String,
    /// Retry hint.
    retryable: bool,
    /// Who should act on the error.
    owner: ErrorOwner,
    /// Raw driver cause — surfaced gated, in `debug.raw`.
    raw: Option<String>,
    /// JS stack trace, when available.
    stack: Option<String>,
    /// Structured, ungated machine context (e.g. `{sqlstate}` / `{http_status}`).
    details: Option<Value>,
}

/// The `__runlet` tag deserialized from a thrown capability error (read in one
/// `json_stringify` + parse rather than field-by-field).
#[derive(Debug, Deserialize)]
struct CapabilityTag {
    /// Raw driver cause.
    #[serde(default)]
    error: Option<String>,
    /// Stable machine code.
    code: String,
    /// Retry hint.
    #[serde(default)]
    retryable: bool,
    /// Originating capability (lowercase, parsed via [`ErrorSource::parse`]).
    source: String,
    /// Responsible owner (lowercase, parsed via [`ErrorOwner::parse`]).
    #[serde(default)]
    owner: Option<String>,
    /// Structured machine context.
    #[serde(default)]
    details: Option<Value>,
}

/// Runs the script in a sandboxed context.
///
/// # Errors
///
/// Returns [`EngineError::Internal`] only for a failure so early there is no outcome to
/// carry (context creation). Every in-execution failure is an [`ExecOutcome::Error`].
pub(crate) fn run(params: &ExecParams<'_>) -> Result<ExecResult, EngineError> {
    let timed_out = setup_timeout(params.runtime, params.timeout);

    let ctx = Context::full(params.runtime).map_err(EngineError::internal)?;

    // Per-invocation `emit` buffer: native `__emit` appends `(kind, value_json)` pairs here,
    // drained into `ExecResult.effects` after execution. The value is opaque to the core (the
    // consumer interprets it); the `kind` is a routing tag surfaced structurally.
    let effects: EmitBuffer = Arc::new(Mutex::new(Vec::new()));

    // Per-invocation `log` buffer: native `__log` appends captured diagnostic entries here, drained
    // into `ExecResult.logs` after execution (both paths). Outside the reproducible outputs (D8).
    let logs: LogBuffer = Arc::new(Mutex::new(LogAccumulator::default()));
    // Execution start, for the relative `offset_us` — attached only under the full profile (the
    // deterministic profile strips the clock, D8), so timing never enters a reproducible run.
    let log_start = Instant::now();
    let log_timing = params.profile == Profile::Full;

    // In-engine capability metric collectors (`http`/`s3` only); the driver-backed capabilities
    // record into the egress adapter, drained by the consumer.
    #[cfg(feature = "http")]
    let mut http_collector: Option<Collector<HttpMetric>> = None;
    #[cfg(feature = "s3")]
    let mut s3_collector: Option<Collector<S3Metric>> = None;

    let js_result = ctx.with(|qctx| -> Result<ExecOutcome, EngineError> {
        inject_bridge(&qctx).map_err(EngineError::internal)?;
        decimal::inject_decimal(&qctx).map_err(EngineError::internal)?;
        // `$` / `money` composes over `__decimal`, so it must follow the `Decimal` injection.
        // Pure (no I/O), so injected under both profiles like `Decimal`.
        money::inject_money(&qctx, params.default_currency).map_err(EngineError::internal)?;
        sys::inject_sys(&qctx, params.sys_config).map_err(EngineError::internal)?;
        inject_emit(&qctx, &effects, params.max_ops, params.max_emit_kind_len)
            .map_err(EngineError::internal)?;
        // `log.*` is injected under both profiles (D8) — logging a deterministic run is allowed;
        // only the timing is withheld there. The floor + D7 bounds ride in via `LogLimits`.
        inject_log(
            &qctx,
            &logs,
            LogLimits {
                floor: params.log_level,
                max_entries: params.max_log_entries,
                max_entry_bytes: params.max_log_entry_bytes,
                max_total_bytes: params.max_log_total_bytes,
            },
            log_start,
            log_timing,
        )
        .map_err(EngineError::internal)?;
        // The capability mux + its wrappers are I/O, so gated to `Profile::Full` exactly like the
        // in-engine capabilities — the boundary is enforced here, never trusted to the caller's
        // `Invocation` (D9: the deterministic profile removes this authority, it is not gated).
        if params.profile == Profile::Full {
            inject_registry(&qctx, params).map_err(EngineError::internal)?;
        }
        #[cfg(feature = "_io")]
        inject_apis(
            &qctx,
            params,
            #[cfg(feature = "http")]
            &mut http_collector,
            #[cfg(feature = "s3")]
            &mut s3_collector,
        )?;
        let handler = match resolve_handler(&qctx, params) {
            Ok(func) => func,
            Err(outcome) => return Ok(outcome),
        };
        invoke_handler(
            &qctx,
            &handler,
            params.context_json,
            &timed_out,
            params.timeout,
        )
    });

    // Cleanup: clear interrupt handler so pooled runtime is clean.
    params.runtime.set_interrupt_handler(None);

    let outcome = enforce_output_cap(
        js_result.unwrap_or_else(ExecOutcome::Error),
        params.max_output_size,
    );

    Ok(ExecResult {
        outcome,
        effects: drain_effects(&effects),
        logs: drain_logs(&logs),
        #[cfg(feature = "http")]
        http_metrics: sandbox::drain(http_collector.as_ref()),
        #[cfg(feature = "s3")]
        s3_metrics: sandbox::drain(s3_collector.as_ref()),
    })
}

/// Enforces the output-size ceiling on a successful result: a handler JSON larger than
/// `max_output_size` (when non-zero) is turned into an [`EngineError::OutputTooLarge`] so a
/// script can't return a `memory_limit`-sized blob. Errors and the disabled case (`0`) pass
/// through untouched.
fn enforce_output_cap(outcome: ExecOutcome, max_output_size: usize) -> ExecOutcome {
    if max_output_size == 0 {
        return outcome;
    }
    if let ExecOutcome::Success(json) = &outcome {
        let size = json.len();
        if size > max_output_size {
            return ExecOutcome::Error(EngineError::OutputTooLarge {
                size,
                limit: max_output_size,
            });
        }
    }
    outcome
}

// -- Setup helpers ----------------------------------------------------------

/// Configures the timeout interrupt handler. Returns the shared flag.
fn setup_timeout(runtime: &Runtime, timeout: Duration) -> Arc<AtomicBool> {
    let timed_out = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&timed_out);
    let start = Instant::now();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let exceeded = start.elapsed() > timeout;
        if exceeded {
            flag.store(true, Ordering::Relaxed);
        }
        exceeded
    })));
    timed_out
}

/// Injects the always-present JS primitives: the `json(data, error)` bridge and the shared
/// `__ffi` FFI unwrap (both egress wrappers depend on `__ffi`, which is why it lands here rather
/// than in the independently-gated `io`/`s3` injection paths).
fn inject_bridge(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let bridge: JsValue<'_> = qctx.eval(JSON_BRIDGE)?;
    drop(bridge);
    let ffi: JsValue<'_> = qctx.eval(FFI_PRIMITIVES)?;
    drop(ffi);
    Ok(())
}

/// Injects the in-engine capabilities `http`/`s3` (subject to the profile).
///
/// These are the enumerated mux-bypass surface (D9): they carry their own in-engine code
/// (`http`'s SSRF-guarded client, `s3`'s `SigV4` signing) rather than routing through the egress
/// mux, and each returns a metric collector captured into the `*_collector` slots. The
/// driver-backed capabilities inject their JS wrappers through the capability registry
/// (`inject_registry`), not here.
#[cfg(feature = "_io")]
fn inject_apis(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
    #[cfg(feature = "http")] http_collector: &mut Option<Collector<HttpMetric>>,
    #[cfg(feature = "s3")] s3_collector: &mut Option<Collector<S3Metric>>,
) -> Result<(), EngineError> {
    // Profile enforcement: the deterministic tier gets **no** I/O capability, regardless of
    // what configs an `Invocation` carries — the boundary is enforced here, not trusted to
    // the author (only `$`/`$sys`, `emit`, and the read-hook remain, injected elsewhere).
    if params.profile != Profile::Full {
        return Ok(());
    }
    #[cfg(feature = "http")]
    if !params.allowed_hosts.is_empty() {
        *http_collector = Some(
            http::inject_http(
                qctx,
                params.allowed_hosts,
                params.max_ops,
                params.allow_private_targets,
                params.wildcard_hosts_allowed,
            )
            .map_err(EngineError::internal)?,
        );
    }
    #[cfg(feature = "s3")]
    if let Some(s3_cfg) = params.s3_config {
        *s3_collector = Some(
            s3::inject_s3(qctx, s3_cfg, params.max_ops, params.allow_private_targets)
                .map_err(EngineError::internal)?,
        );
    }
    Ok(())
}

/// Evaluates the user script.
fn eval_script(qctx: &Ctx<'_>, script: &str) -> Result<(), rquickjs::Error> {
    let result: JsValue<'_> = qctx.eval(script)?;
    drop(result);
    Ok(())
}

/// Removes `eval` and `Proxy` before the handler runs.
///
/// This is isolation hardening, **not** a dynamic-code block: `new Function("…")()` and the
/// `AsyncFunction`/`GeneratorFunction` constructors still compile strings, and that is fine —
/// the script is already arbitrary code, and the real boundary is `QuickJS` having no host
/// access (no fs/net/process). `eval` is removed to trim a historically bug-prone surface and
/// `Proxy` to deny exotic-object traps over the injected capability/`$sys` globals. Do not
/// rely on their absence for any policy that depends on the script *not* generating code.
fn sanitize_globals(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let globals = qctx.globals();
    globals.remove("eval")?;
    globals.remove("Proxy")?;
    Ok(())
}

/// Evaluates the user source (ES module or classic script), sanitizes globals, and returns
/// the handler function. On failure returns the classified error outcome to short-circuit:
/// a syntax/import error, or `HANDLER_NOT_DEFINED` when no handler is exported/defined.
///
/// Module vs script is detected by a top-level `export` ([`is_es_module`]); the handler
/// body runs *after* `sanitize_globals` either way, so the two modes share the same
/// `eval`/`Proxy`-removed execution environment.
fn resolve_handler<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Function<'js>, ExecOutcome> {
    let (script, profile) = (params.script, params.profile);
    if is_es_module(script) {
        let module = eval_module(qctx, params).map_err(ExecOutcome::Error)?;
        harden(qctx, profile).map_err(|err| ExecOutcome::Error(EngineError::internal(err)))?;
        module_handler(&module).ok_or(ExecOutcome::Error(EngineError::HandlerNotDefined))
    } else {
        eval_script(qctx, script).map_err(|_err| ExecOutcome::Error(classify_eval_error(qctx)))?;
        harden(qctx, profile).map_err(|err| ExecOutcome::Error(EngineError::internal(err)))?;
        qctx.globals()
            .get::<_, Function<'js>>("handler")
            .map_err(|_err| ExecOutcome::Error(EngineError::HandlerNotDefined))
    }
}

/// Hardens the execution environment after the user source is evaluated, before the handler
/// runs: removes `eval`/`Proxy` (always) and, under [`Profile::Deterministic`], neutralizes
/// nondeterministic surfaces on top.
fn harden(qctx: &Ctx<'_>, profile: Profile) -> Result<(), rquickjs::Error> {
    sanitize_globals(qctx)?;
    if profile == Profile::Deterministic {
        sanitize_determinism(qctx)?;
    }
    Ok(())
}

/// Neutralizes nondeterminism for [`Profile::Deterministic`]: overrides `Math.random`,
/// `Date.now`, zero-arg `new Date()`, `$sys.date.now`, and `$sys.crypto.uuid` to throw
/// (see `js/determinism.js`). Runs after [`sanitize_globals`].
fn sanitize_determinism(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let sanitized: JsValue<'_> = qctx.eval(DETERMINISM_SANITIZER)?;
    drop(sanitized);
    Ok(())
}

/// Injects the `emit(kind, value)` host function: it `JSON.stringify`s the value and appends a
/// `(kind, value_json)` pair to the per-invocation `effects` buffer (surfaced as
/// `Outcome.effects`). `kind` is a required non-empty string tag bounded by `max_emit_kind_len`
/// (an over-long or empty tag is rejected and records nothing); the number of effects is capped
/// at `max_ops` so a handler can't grow the buffer without bound. The `value` is opaque to the
/// core — the consumer interprets it ("logic proposes, the engine disposes"); the `kind` is a
/// routing/governance tag the core surfaces but never interprets.
fn inject_emit(
    qctx: &Ctx<'_>,
    effects: &EmitBuffer,
    max_ops: usize,
    max_emit_kind_len: usize,
) -> Result<(), rquickjs::Error> {
    let buffer = Arc::clone(effects);
    let emit_fn = Function::new(qctx.clone(), move |kind: String, value: String| -> String {
        if kind.is_empty() {
            return "emit(kind, value): kind must be a non-empty string".to_owned();
        }
        if kind.chars().count() > max_emit_kind_len {
            return format!(
                "emit(kind, value): kind exceeds the {max_emit_kind_len}-character limit"
            );
        }
        match buffer.lock() {
            Ok(buf) if buf.len() >= max_ops => {
                format!("too many emit() calls: limit is {max_ops} per execution")
            }
            Ok(mut buf) => {
                buf.push((kind, value));
                String::new()
            }
            Err(_poisoned) => "emit buffer unavailable".to_owned(),
        }
    })?
    .with_name("__emit")?;
    qctx.globals().set("__emit", emit_fn)?;
    // `emit(kind, v)` type-checks `kind`, stringifies `v`, and forwards both; a non-empty return
    // is an error the wrapper throws (the native side re-checks non-empty + the length bound).
    let wrapper: JsValue<'_> = qctx.eval(
        "globalThis.emit = function (kind, value) { \
           if (typeof kind !== 'string' || kind.length === 0) \
             throw new Error('emit(kind, value): kind must be a non-empty string'); \
           var err = __emit(kind, JSON.stringify(value === undefined ? null : value)); \
           if (err) throw new Error(err); \
         };",
    )?;
    drop(wrapper);
    Ok(())
}

/// The `{"truncated":true}` property marker substituted for an oversize entry's real properties
/// (D7), analogous to Cloudflare Workers' `$cloudflare.truncated`.
const LOG_TRUNCATED_MARKER: &str = "{\"truncated\":true}";

/// Injects the `log.*` diagnostic channel.
///
/// Registers the native `__log(level, template, properties_json, message)` sink plus the
/// `__logFloor` numeric floor the JS wrapper (`js/log.js`) checks *before* serializing (D6 — a
/// below-floor call is near-free). Each accepted call captures a structured entry into the
/// per-invocation `logs` buffer under the D7 triad (count / per-entry / total bounds), assigning a
/// monotonic `seq` and, under the full profile, a relative `offset_us`. The core surfaces the
/// structure but never interprets a log's meaning; routing to a sink is the consumer's job
/// (edge-side).
fn inject_log(
    qctx: &Ctx<'_>,
    logs: &LogBuffer,
    limits: LogLimits,
    start: Instant,
    record_timing: bool,
) -> Result<(), rquickjs::Error> {
    // The numeric level floor the JS wrapper compares against before building an entry (D6).
    qctx.globals()
        .set("__logFloor", i32::from(limits.floor.ordinal()))?;
    let buffer = Arc::clone(logs);
    let log_fn = Function::new(
        qctx.clone(),
        move |level: String,
              template: String,
              properties_json: String,
              message: String|
              -> String {
            // The wrapper always passes a valid lowercase level; fall back to `info` defensively.
            let parsed_level = LogLevel::parse(&level).unwrap_or(LogLevel::Info);
            let offset_us = record_timing.then(|| start.elapsed().as_micros());
            match buffer.lock() {
                Ok(mut acc) => {
                    capture_log(
                        &mut acc,
                        &limits,
                        PendingLog {
                            level: parsed_level,
                            template,
                            properties_json,
                            message,
                            offset_us,
                        },
                    );
                    String::new()
                }
                Err(_poisoned) => "log buffer unavailable".to_owned(),
            }
        },
    )?
    .with_name("__log")?;
    qctx.globals().set("__log", log_fn)?;
    let wrapper: JsValue<'_> = qctx.eval(LOG_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

/// Captures one accepted `log` call into the accumulator under the D7 triad.
///
/// A call beyond the count cap, or one whose size would push the running total past
/// `max_total_bytes` (the total binds first), is silently dropped (records nothing, never errors —
/// the execution is otherwise unaffected). An entry over `max_entry_bytes` is truncated: its
/// properties become the `{"truncated":true}` marker and its message is trimmed to a char boundary
/// within the per-entry budget.
fn capture_log(acc: &mut LogAccumulator, limits: &LogLimits, pending: PendingLog) {
    if acc.entries.len() >= limits.max_entries {
        return; // count cap: drop, record nothing
    }
    let PendingLog {
        level,
        template,
        properties_json,
        message,
        offset_us,
    } = pending;
    let raw_size = template
        .len()
        .saturating_add(properties_json.len())
        .saturating_add(message.len());
    let (props, msg) = if raw_size > limits.max_entry_bytes {
        // Truncate: replace the properties with the marker and trim the message to fit the
        // per-entry budget left after the template + marker.
        let budget = limits
            .max_entry_bytes
            .saturating_sub(template.len())
            .saturating_sub(LOG_TRUNCATED_MARKER.len());
        (
            LOG_TRUNCATED_MARKER.to_owned(),
            truncate_on_char_boundary(&message, budget),
        )
    } else {
        (properties_json, message)
    };
    let entry_size = template
        .len()
        .saturating_add(props.len())
        .saturating_add(msg.len());
    if acc.total_bytes.saturating_add(entry_size) > limits.max_total_bytes {
        return; // total cap (binds first): drop, record nothing
    }
    acc.total_bytes = acc.total_bytes.saturating_add(entry_size);
    let seq = u64::try_from(acc.entries.len()).unwrap_or(u64::MAX);
    acc.entries.push(RawLog {
        level,
        template,
        properties_json: props,
        message: msg,
        seq,
        offset_us,
    });
}

/// Truncates `text` to at most `max_bytes` bytes, ending on a UTF-8 char boundary so the result is
/// always valid. Returns the whole string when it already fits.
fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or("").to_owned()
}

/// Drains the per-invocation `log` buffer into public [`LogEntry`]s. Each `properties_json` was
/// produced by `JSON.stringify` (or is the truncation marker), so it parses; a (theoretically
/// impossible) parse failure drops that entry rather than aborting the outcome. Runs after
/// execution on both paths (capture-on-failure, D8).
fn drain_logs(logs: &LogBuffer) -> Vec<LogEntry> {
    let Ok(acc) = logs.lock() else {
        return Vec::new();
    };
    acc.entries
        .iter()
        .filter_map(|raw| {
            RawValue::from_string(raw.properties_json.clone())
                .ok()
                .map(|properties| LogEntry {
                    level: raw.level,
                    template: raw.template.clone(),
                    properties,
                    message: raw.message.clone(),
                    seq: raw.seq,
                    offset_us: raw.offset_us,
                })
        })
        .collect()
}

/// Injects the capability mux (`io.call` via `__io`) plus each enabled registered capability's
/// JS wrapper.
///
/// The mux routes by name through the registry (local backend → per-request fallback → builder
/// fallback), applies the SSRF guard to `ScriptControlled` capabilities, and fails closed. It is
/// injected only when there is I/O to do — an active registry or a per-request fallback egress;
/// otherwise the `io` global is withheld entirely. A registered def's wrapper is `eval`'d only if
/// its name is in `enabled_io` (per-request, opt-in).
fn inject_registry(qctx: &Ctx<'_>, params: &ExecParams<'_>) -> Result<(), rquickjs::Error> {
    let registry = params.registry.cloned().unwrap_or_default();
    if !registry.is_active() && params.egress.is_none() {
        return Ok(());
    }
    inject_mux(qctx, registry.clone(), params)?;
    for def in registry.defs() {
        if params.enabled_io.contains(&def.name()) {
            let wrapper: JsValue<'_> = qctx.eval(def.js_wrapper())?;
            drop(wrapper);
        }
    }
    Ok(())
}

/// Injects the native `__io(name, action, payload_json)` + the `io.call` JS wrapper (`js/io.js`).
///
/// `__io` forwards to [`CapabilityRegistry::dispatch`] and returns either the backend JSON
/// verbatim or a `__runlet` tagged error; the JS wrapper throws on the latter so the engine
/// classifies it as a capability error. Calls are capped at `max_ops` per execution (a shared
/// counter, mirroring `emit`), so the mux can't be used to bypass the op budget.
fn inject_mux(
    qctx: &Ctx<'_>,
    registry: CapabilityRegistry,
    params: &ExecParams<'_>,
) -> Result<(), rquickjs::Error> {
    let fallback = params.egress.clone();
    let max_ops = params.max_ops;
    let allow_private = params.allow_private_targets;
    // The per-request `config.io` allowlist: `io.call(name, …)` is gated by it centrally, so an
    // unlisted name is rejected (`RESOURCE_NOT_FOUND`) before it can reach any backend (D3).
    let allowed: Vec<String> = params
        .enabled_io
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let used = Arc::new(AtomicUsize::new(0));
    let io_fn = Function::new(
        qctx.clone(),
        move |name: String, action: String, payload: String| -> String {
            // Allowlist gate (D3): only a name the request listed in `config.io` may be addressed;
            // an unlisted name is rejected before any backend, metering, or op-budget spend.
            if !allowed.iter().any(|listed| listed == &name) {
                return errors::dynamic_fault_json(&errors::DynamicFault {
                    error: "resource is not in the request's io allowlist",
                    code: "RESOURCE_NOT_FOUND",
                    retryable: false,
                    owner: ErrorOwner::Developer,
                    source: &name,
                    details: None,
                });
            }
            if used.load(Ordering::Relaxed) >= max_ops {
                let message = format!("too many operations: limit is {max_ops} per execution");
                return errors::dynamic_fault_json(&errors::DynamicFault {
                    error: &message,
                    code: "IO_OP_LIMIT",
                    retryable: false,
                    owner: ErrorOwner::Developer,
                    source: "engine",
                    details: None,
                });
            }
            let _prev = used.fetch_add(1, Ordering::Relaxed);
            registry.dispatch(&MuxCall {
                name: &name,
                action: &action,
                payload: &payload,
                allow_private,
                fallback: fallback.as_ref(),
            })
        },
    )?
    .with_name("__io")?;
    qctx.globals().set("__io", io_fn)?;
    let wrapper: JsValue<'_> = qctx.eval(IO_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

/// Drains the per-invocation `emit` buffer into tagged [`Effect`]s. Each value was produced by
/// `JSON.stringify`, so it parses; a (theoretically impossible) parse failure drops that entry
/// rather than aborting the whole outcome. Runs after execution on both the success and error
/// paths, so effects emitted before a handler throws are still captured (capture-on-failure).
fn drain_effects(effects: &EmitBuffer) -> Vec<Effect> {
    let Ok(buf) = effects.lock() else {
        return Vec::new();
    };
    buf.iter()
        .filter_map(|(kind, value_json)| {
            RawValue::from_string(value_json.clone())
                .ok()
                .map(|value| Effect {
                    kind: kind.clone(),
                    value,
                })
        })
        .collect()
}

/// Best-effort detection of ES-module source by a top-level `export` — the syntax a
/// handler-module must use to export its handler. A miss is self-correcting: script-mode
/// on a real module fails to parse (a syntax error), module-mode on a plain script finds
/// no exported handler (`HANDLER_NOT_DEFINED`) — never a silent wrong result.
fn is_es_module(script: &str) -> bool {
    script.lines().any(|line| {
        line.trim_start()
            .strip_prefix("export")
            .is_some_and(|rest| rest.starts_with([' ', '\t', '{', '*']))
    })
}

/// Evaluates the user source as an ES module, settling synchronously: `Promise::finish`
/// pumps the job queue to completion, and since every jsbox capability is sync FFI a module
/// never truly suspends. Imports resolve through the per-runtime registry loader. On failure
/// the pending exception is classified into a [`EngineError`] (`MODULE_NOT_FOUND` for an
/// unresolved `import`, else a syntax/top-level error).
fn eval_module<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Module<'js, Evaluated>, EngineError> {
    try_eval_module(qctx, params).map_err(|()| classify_module_error(qctx))
}

/// The raw eval attempt; `Err(())` leaves the pending exception set for classification.
fn try_eval_module<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Module<'js, Evaluated>, ()> {
    let declared = obtain_declared(qctx, params)?;
    let (module, promise) = declared.eval().map_err(drop)?;
    promise.finish::<()>().map_err(drop)?;
    Ok(module)
}

/// Obtains the declared (compiled-but-not-evaluated) handler module: a plain
/// `Module::declare` (parse + compile) when no bytecode cache is wired, or — with a cache
/// present — a `Module::load` of previously-compiled bytecode on a hit, and on a miss a compile
/// that is admitted to the cache only if the source clears the size floor. Either way the
/// returned module is then evaluated by the caller, so behavior (including thrown syntax/
/// top-level errors, which are classified from the pending exception) is identical to the
/// uncached path.
fn obtain_declared<'js>(
    qctx: &Ctx<'js>,
    params: &ExecParams<'_>,
) -> Result<Module<'js, Declared>, ()> {
    let Some(cache) = params.bytecode_cache else {
        return Module::declare(qctx.clone(), "handler", params.script).map_err(drop);
    };
    let namespace = params.cache_namespace.unwrap_or("");
    let key = bytecode::digest(namespace.as_bytes(), params.script.as_bytes());
    if let Some(bytecode) = cache.get(&key) {
        cache.note_hit();
        return load_bytecode(qctx, &bytecode);
    }
    cache.note_miss();
    let declared = Module::declare(qctx.clone(), "handler", params.script).map_err(drop)?;
    // Autonomous, size-based admission: cache only scripts large enough to be worth it (small
    // handlers recompile every call and never touch the `unsafe` load path). A failed
    // serialization just forgoes caching this script — it never fails the request.
    if cache.should_store(params.script.len())
        && let Ok(bytes) = declared.write(WriteOptions::default())
    {
        cache.insert(key, Arc::from(bytes.into_boxed_slice()));
    }
    Ok(declared)
}

/// Loads a handler module from previously-cached `QuickJS` bytecode.
///
/// The lone `unsafe` in the workspace: `Module::load` is `unsafe` because it trusts the bytes
/// are valid bytecode (malformed input is UB). Here the bytes are *self-produced* — they came
/// only from `Module::write` on a module this same process compiled from source, held in an
/// in-memory `BytecodeCache`, never crossing a trust boundary or a `QuickJS`-version boundary.
#[expect(
    unsafe_code,
    reason = "Module::load deserializes self-produced, in-process bytecode (see fn docs); the \
              bytes originate only from our own Module::write earlier this process"
)]
fn load_bytecode<'js>(qctx: &Ctx<'js>, bytecode: &[u8]) -> Result<Module<'js, Declared>, ()> {
    // SAFETY: `bytecode` was produced by `Module::write` on a module compiled in this process
    // and stored verbatim in the cache; it is therefore valid QuickJS bytecode for this build.
    unsafe { Module::load(qctx.clone(), bytecode) }.map_err(drop)
}

/// Classifies a module eval failure from the pending exception: the resolver's
/// [`modules::UNRESOLVED_MARKER`] in the message ⇒ `MODULE_NOT_FOUND` (a bad `import`),
/// otherwise a syntax / top-level error. Consumes the pending exception.
fn classify_module_error(qctx: &Ctx<'_>) -> EngineError {
    let caught = qctx.catch();
    let message = caught
        .as_object()
        .and_then(|obj| read_str_prop(obj, "message"));
    match message {
        Some(msg) if msg.contains(modules::UNRESOLVED_MARKER) => EngineError::ModuleNotFound(msg),
        Some(msg) => EngineError::Syntax(msg),
        None => EngineError::Syntax("syntax error".to_owned()),
    }
}

/// Reads the exported handler from an evaluated module: `export default function handler`
/// (namespace `default`) is preferred, then `export function handler` (namespace `handler`).
/// `None` if neither names an exported function.
fn module_handler<'js>(module: &Module<'js, Evaluated>) -> Option<Function<'js>> {
    let namespace = module.namespace().ok()?;
    namespace
        .get::<_, Function<'js>>("default")
        .ok()
        .or_else(|| namespace.get::<_, Function<'js>>("handler").ok())
}

// -- Handler invocation + classification ------------------------------------

/// Calls the resolved `handler(context)` and classifies the outcome.
fn invoke_handler<'js>(
    qctx: &Ctx<'js>,
    handler: &Function<'js>,
    context_json: &str,
    timed_out: &AtomicBool,
    timeout: Duration,
) -> Result<ExecOutcome, EngineError> {
    // The context is already syntactically valid JSON (validated as `RawValue` at the
    // HTTP layer), so the only realistic `json_parse` failure is the object graph
    // exceeding the sandbox memory limit. Surface it as a clean `MemoryLimit` (422)
    // rather than an `Internal` (500) server fault. The config invariant
    // (`max_context_size <= memory_limit / 4`) keeps this path unreachable in practice.
    let parsed_ctx: JsValue<'_> = match qctx.json_parse(context_json) {
        Ok(value) => value,
        Err(_parse_err) => {
            drop(qctx.catch()); // consume the pending exception before returning
            return Ok(ExecOutcome::Error(EngineError::MemoryLimit));
        }
    };

    match handler.call::<_, JsValue<'_>>((parsed_ctx,)) {
        Ok(value) => {
            let json = extract_json_string(qctx, value).map_err(EngineError::internal)?;
            Ok(ExecOutcome::Success(json))
        }
        Err(_call_err) => Ok(ExecOutcome::Error(classify_throw(qctx, timed_out, timeout))),
    }
}

/// Classifies a failed `eval` (syntax / top-level error) using the pending exception.
fn classify_eval_error(qctx: &Ctx<'_>) -> EngineError {
    let caught = qctx.catch();
    let message = caught
        .as_object()
        .and_then(|obj| read_str_prop(obj, "message"))
        .unwrap_or_else(|| "syntax error".to_owned());
    EngineError::Syntax(message)
}

/// Classifies a handler throw structurally (timeout flag → `__runlet` tag → script),
/// without parsing message text. Out-of-memory is handled earlier, at context parse
/// (see [`call_handler`]) — a handler that over-allocates instead surfaces as a script
/// error, which correctly attributes it to the developer's code.
fn classify_throw(qctx: &Ctx<'_>, timed_out: &AtomicBool, timeout: Duration) -> EngineError {
    if timed_out.load(Ordering::Relaxed) {
        return EngineError::Timeout {
            limit_ms: timeout.as_millis(),
        };
    }

    let caught = qctx.catch();
    let Some(obj) = caught.as_object() else {
        return EngineError::Script {
            message: stringify_value(&caught),
            stack: None,
        };
    };

    let stack = read_str_prop(obj, "stack");

    if let Some(cap) = read_capability_tag(qctx, obj, stack.clone()) {
        return EngineError::Capability(Box::new(cap));
    }
    let message = read_str_prop(obj, "message").unwrap_or_default();
    EngineError::Script { message, stack }
}

/// Reads a capability's `__runlet` tag, if present and well-formed.
///
/// Stringifies the tag object once and deserializes it (cleaner than field-by-field,
/// and `details` comes back as a `serde_json::Value` for free). Returns `None` if the
/// tag is absent or names no known source → the throw is treated as a script error.
fn read_capability_tag<'js>(
    qctx: &Ctx<'js>,
    obj: &Object<'js>,
    stack: Option<String>,
) -> Option<CapabilityErr> {
    let tag_val = obj.get::<_, JsValue<'js>>("__runlet").ok()?;
    if tag_val.is_undefined() || tag_val.is_null() {
        return None;
    }
    let stringified = qctx.json_stringify(tag_val).ok().flatten()?;
    let json = stringified.to_string().ok()?;
    let tag: CapabilityTag = serde_json::from_str(&json).ok()?;

    let source = ErrorSource::parse(&tag.source)?;
    let owner = tag
        .owner
        .as_deref()
        .and_then(ErrorOwner::parse)
        .unwrap_or(ErrorOwner::Operator);
    Some(CapabilityErr {
        source,
        code: tag.code,
        retryable: tag.retryable,
        owner,
        raw: tag.error,
        stack,
        details: tag.details,
    })
}

/// Reads a non-empty string property off a JS object.
fn read_str_prop(obj: &Object<'_>, key: &str) -> Option<String> {
    obj.get::<_, String>(key)
        .ok()
        .filter(|text| !text.is_empty())
}

/// Best-effort string for a thrown non-object value (`throw "x"` / `throw 42`).
fn stringify_value(value: &JsValue<'_>) -> String {
    value
        .as_string()
        .and_then(|js_str| js_str.to_string().ok())
        .unwrap_or_else(|| "script error".to_owned())
}

/// Extracts a JSON string from a JS value — single copy across FFI.
fn extract_json_string<'js>(
    qctx: &Ctx<'js>,
    result: JsValue<'js>,
) -> Result<String, rquickjs::Error> {
    if let Some(js_str) = result.as_string() {
        return js_str.to_string();
    }
    let stringified = qctx.json_stringify(result)?;
    stringified.map_or_else(
        || Ok("{\"data\":null,\"error\":null}".into()),
        |js_str| js_str.to_string(),
    )
}

// -- Error → envelope assembly ----------------------------------------------

/// Builds a `runtime`-category envelope (source = engine) with a safe message.
fn runtime_envelope(
    code: &str,
    retryable: bool,
    owner: ErrorOwner,
    message: String,
) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCategory::Runtime,
        ErrorSource::Engine,
        code.to_owned(),
        retryable,
        owner,
    )
    .with_message(message)
}

/// Builds a `script`-category envelope, attaching the stack when debug is on. The
/// message is the developer's own JS error message (their code, not secret).
fn script_envelope(message: String, stack: Option<String>, error_debug: bool) -> ErrorEnvelope {
    let envelope = ErrorEnvelope::new(
        ErrorCategory::Script,
        ErrorSource::Handler,
        "SCRIPT_ERROR".to_owned(),
        false,
        ErrorOwner::Developer,
    )
    .with_message(message);
    attach_debug(envelope, stack, None, error_debug)
}

/// Attaches gated debug context (`stack` + raw cause). Omitted entirely when
/// `error_debug` is off or there is nothing to carry.
fn attach_debug(
    envelope: ErrorEnvelope,
    stack: Option<String>,
    raw: Option<String>,
    error_debug: bool,
) -> ErrorEnvelope {
    if error_debug {
        envelope.with_debug(ErrorDebug { stack, raw })
    } else {
        envelope
    }
}

/// A generic, secret-free message per capability — keeps raw driver text (which can
/// contain credentials / PII) out of the always-present `message`.
const fn capability_message(source: ErrorSource) -> &'static str {
    match source {
        ErrorSource::Db | ErrorSource::Mongo => "database request failed",
        ErrorSource::Mail => "mail delivery failed",
        ErrorSource::S3 => "object storage request failed",
        ErrorSource::Api => "upstream request failed",
        ErrorSource::Redis => "redis request failed",
        ErrorSource::Amq => "message broker request failed",
        ErrorSource::Auth => "identity request failed",
        ErrorSource::Request | ErrorSource::Engine | ErrorSource::Handler => {
            "capability request failed"
        }
    }
}

impl EngineError {
    /// Wraps any Rust-side failure as an internal error.
    fn internal<E: Display>(err: E) -> Self {
        Self::Internal(err.to_string())
    }

    /// Assembles the structured [`ErrorEnvelope`], gating debug on `error_debug`.
    ///
    /// `timeout_retryable` sets the `retryable` of a wall-clock `TIMEOUT` (the operator knob —
    /// the engine cannot tell a slow dependency from a slow algorithm). `MEMORY_LIMIT` and the
    /// op-count cap are deterministic for a given `(script, input)` and stay non-retryable
    /// regardless. The status *projection* over the resulting envelope lives with the consumer.
    #[must_use]
    pub fn into_envelope(self, error_debug: bool, timeout_retryable: bool) -> ErrorEnvelope {
        let dev = ErrorOwner::Developer;
        match self {
            Self::Syntax(message) => runtime_envelope("SYNTAX_ERROR", false, dev, message),
            // Request-category (the caller named a key that doesn't exist) — mirrors the
            // HTTP front's own `SCRIPT_NOT_FOUND` envelope so the two paths are identical.
            Self::ScriptNotFound(message) => ErrorEnvelope::new(
                ErrorCategory::Request,
                ErrorSource::Request,
                "SCRIPT_NOT_FOUND".to_owned(),
                false,
                ErrorOwner::Caller,
            )
            .with_message(message),
            Self::ModuleNotFound(message) => {
                runtime_envelope("MODULE_NOT_FOUND", false, dev, message)
            }
            Self::HandlerNotDefined => runtime_envelope(
                "HANDLER_NOT_DEFINED",
                false,
                dev,
                HANDLER_MISSING_MSG.to_owned(),
            ),
            Self::Timeout { limit_ms } => runtime_envelope(
                "TIMEOUT",
                timeout_retryable,
                dev,
                format!("execution timed out ({limit_ms}ms limit)"),
            ),
            Self::MemoryLimit => {
                runtime_envelope("MEMORY_LIMIT", false, dev, MEMORY_MSG.to_owned())
            }
            Self::Malformed(message) => runtime_envelope("MALFORMED_RESPONSE", false, dev, message),
            Self::OutputTooLarge { size, limit } => runtime_envelope(
                "OUTPUT_TOO_LARGE",
                false,
                dev,
                format!("handler output too large: {size} bytes (max {limit})"),
            ),
            // Generic message + raw cause in gated debug — never leak internal infra
            // detail (hostnames, etc.) in the always-present `message`.
            Self::Internal(raw) => attach_debug(
                runtime_envelope(
                    "INTERNAL",
                    true,
                    ErrorOwner::Operator,
                    "internal error".to_owned(),
                ),
                None,
                Some(raw),
                error_debug,
            ),
            Self::ShuttingDown => runtime_envelope(
                "SHUTTING_DOWN",
                true,
                ErrorOwner::Operator,
                "service is shutting down".to_owned(),
            ),
            Self::Script { message, stack } => script_envelope(message, stack, error_debug),
            Self::Capability(cap) => cap.into_envelope(error_debug),
        }
    }
}

impl CapabilityErr {
    /// Assembles a `capability`-category envelope: generic message, structured details,
    /// raw cause + stack in gated debug.
    fn into_envelope(self, error_debug: bool) -> ErrorEnvelope {
        let envelope = ErrorEnvelope::new(
            ErrorCategory::Capability,
            self.source,
            self.code,
            self.retryable,
            self.owner,
        )
        .with_message(capability_message(self.source).to_owned())
        .with_details(self.details);
        attach_debug(envelope, self.stack, self.raw, error_debug)
    }
}

#[cfg(test)]
mod tests {
    //! Verifies the wall-clock interrupt preempts a catastrophic-backtracking regex.
    //!
    //! `QuickJS`'s libregexp does not yield to the interrupt handler on its own, so this proves
    //! that a `ReDoS` pattern is still bounded by the execution timeout rather than pinning a
    //! `spawn_blocking` thread until the match completes.

    use rquickjs::{Context, Runtime, Value as JsValue};
    use std::time::{Duration, Instant};

    /// A `(a+)+$` pattern over a non-matching tail backtracks exponentially; with 30 leading
    /// `a`s it would run for several seconds uninterrupted, so prompt completion proves the
    /// interrupt aborted the match rather than letting it run to the end.
    #[test]
    fn catastrophic_regex_is_interrupted() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let timeout = Duration::from_millis(250);
        let start = Instant::now();
        runtime.set_interrupt_handler(Some(Box::new(move || start.elapsed() > timeout)));
        let ctx = Context::full(&runtime).unwrap_or_else(|_err| unreachable!());
        let script = format!("/(a+)+$/.test(\"{}!\")", "a".repeat(30));
        ctx.with(|qctx| {
            let res: Result<JsValue<'_>, _> = qctx.eval(script.as_bytes());
            assert!(
                res.is_err(),
                "the wall-clock interrupt must abort a catastrophic regex"
            );
        });
        runtime.set_interrupt_handler(None);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "regex was not preempted promptly (interrupt did not fire during matching)"
        );
    }
}

/// The bytecode cache must populate on the first run and produce a byte-identical result on
/// the second (the `Module::load` path), and must autonomously skip caching sources below its
/// size floor. The extra `cfg` gates to the capability-free build so `ExecParams` has no I/O
/// fields to populate; the separate `#[cfg(test)]` keeps `tests_outside_test_module` satisfied.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod bytecode_cache_tests {
    use super::{ExecOutcome, ExecParams, Profile, run};
    use crate::bytecode::BytecodeCache;
    use rquickjs::Runtime;
    use std::time::Duration;

    /// An ES-module handler — only the module path is cached. Adds 1 to `ctx.n`.
    const SCRIPT: &str = "export default function handler(ctx) { return json(ctx.n + 1); }";

    /// Builds `ExecParams` for the minimal (no-capability) build with the cache wired in.
    fn params<'a>(runtime: &'a Runtime, cache: &'a BytecodeCache) -> ExecParams<'a> {
        ExecParams {
            runtime,
            bytecode_cache: Some(cache),
            cache_namespace: None,
            script: SCRIPT,
            context_json: "{\"n\":41}",
            timeout: Duration::from_secs(5),
            profile: Profile::Full,
            sys_config: None,
            registry: None,
            enabled_io: &[],
            egress: None,
            default_currency: None,
            max_ops: 64,
            max_emit_kind_len: 64,
            log_level: super::LogLevel::Info,
            max_log_entries: 256,
            max_log_entry_bytes: 256 * 1024,
            max_log_total_bytes: 1024 * 1024,
            max_output_size: 0,
            allow_private_targets: false,
        }
    }

    /// Extracts the success envelope; a non-success outcome fails the test.
    fn success_json(outcome: ExecOutcome) -> String {
        let ExecOutcome::Success(json) = outcome else {
            unreachable!("expected a success outcome");
        };
        json
    }

    /// With a zero floor (cache everything): the cold run compiles + stores, the warm run loads
    /// bytecode and returns a byte-identical result.
    #[test]
    fn warm_run_loads_bytecode_with_identical_result() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let cache = BytecodeCache::new(8, 0);

        let cold = run(&params(&runtime, &cache)).unwrap_or_else(|_err| unreachable!());
        let cold_json = success_json(cold.outcome);
        let after_cold = cache.stats();
        assert_eq!(after_cold.misses, 1, "cold run compiles (miss)");
        assert_eq!(after_cold.stored, 1, "cold run caches the compiled module");

        let warm = run(&params(&runtime, &cache)).unwrap_or_else(|_err| unreachable!());
        let warm_json = success_json(warm.outcome);
        let after_warm = cache.stats();
        assert_eq!(
            cold_json, warm_json,
            "bytecode-load result matches the compiled result"
        );
        assert!(cold_json.contains("42"), "handler computed ctx.n + 1 = 42");
        assert_eq!(after_warm.hits, 1, "warm run is a cache hit");
        assert_eq!(after_warm.stored, 1, "warm run re-uses, doesn't re-store");
        assert_eq!(after_warm.entries, 1, "exactly one entry cached");
    }

    /// Autonomy: a sub-floor source is never cached — both runs miss and nothing is stored,
    /// so the `unsafe` load path is never exercised for a tiny handler.
    #[test]
    fn small_source_is_never_cached() {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        // Floor far above the ~60-byte SCRIPT, so admission is refused.
        let cache = BytecodeCache::new(8, 4096);

        for _ in 0..2 {
            let outcome = run(&params(&runtime, &cache)).unwrap_or_else(|_err| unreachable!());
            assert!(
                success_json(outcome.outcome).contains("42"),
                "still correct"
            );
        }
        let stats = cache.stats();
        assert_eq!(
            stats.misses, 2,
            "every run recompiles (sub-floor, never cached)"
        );
        assert_eq!(stats.stored, 0, "nothing admitted below the size floor");
        assert_eq!(stats.hits, 0, "no cache hits");
        assert_eq!(stats.entries, 0, "cache stays empty");
    }
}

/// End-to-end `Decimal` (exact number) + `$` / `money` (currency-bound) surface: drives the JS
/// wrappers (`decimal.js` + `money.js`) through the engine so the FFI, the ISO 4217 table, the
/// currency cascade, and serialization are all exercised together. Capability-free build (no
/// in-engine `http`/`s3` fields on `ExecParams`).
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod money_tests {
    use super::{ExecOutcome, ExecParams, Profile, run};
    use rquickjs::Runtime;
    use std::time::Duration;

    /// Runs `script` and returns the success `data`/`error` envelope JSON. `default_currency` seeds
    /// the construction cascade fallback (the resolved `config.currency` else operator default).
    fn run_script(script: &str, default_currency: Option<&str>) -> String {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let params = ExecParams {
            runtime: &runtime,
            bytecode_cache: None,
            cache_namespace: None,
            script,
            context_json: "{}",
            timeout: Duration::from_secs(5),
            profile: Profile::Full,
            sys_config: None,
            registry: None,
            enabled_io: &[],
            egress: None,
            default_currency,
            max_ops: 4096,
            max_emit_kind_len: 64,
            log_level: super::LogLevel::Info,
            max_log_entries: 256,
            max_log_entry_bytes: 256 * 1024,
            max_log_total_bytes: 1024 * 1024,
            max_output_size: 0,
            allow_private_targets: false,
        };
        let result = run(&params).unwrap_or_else(|_err| unreachable!());
        match result.outcome {
            ExecOutcome::Success(json) => json,
            ExecOutcome::Error(err) => format!("ERROR: {err:?}"),
        }
    }

    /// `Decimal` is exact and distinct from `$`; snake_case + deprecated camelCase both resolve.
    #[test]
    fn decimal_is_exact_and_distinct_from_money() {
        let out = run_script(
            "function handler() { return json({ \
               sum: Decimal('0.1').add('0.2').toString(), \
               distinct: Decimal !== $, \
               snake: Decimal('0').is_zero(), \
               camel: Decimal('0').isZero() }); }",
            None,
        );
        assert!(out.contains("\"sum\":\"0.3\""), "exact base-10: {out}");
        assert!(out.contains("\"distinct\":true"), "Decimal !== $: {out}");
        assert!(out.contains("\"snake\":true") && out.contains("\"camel\":true"), "{out}");
    }

    /// `Decimal` bounded helpers + banker's rounding + cash rounding compose in JS.
    #[test]
    fn decimal_helpers_and_modes() {
        let out = run_script(
            "function handler() { return json({ \
               clamp: Decimal('120').clamp(0, 100).toString(), \
               bankers: Decimal('2.5').round(0, 'half_even').toString(), \
               cash: Decimal('2.03').round_to('0.05').toString() }); }",
            None,
        );
        assert!(out.contains("\"clamp\":\"100\""), "{out}");
        assert!(out.contains("\"bankers\":\"2\""), "half_even ties to even: {out}");
        assert!(out.contains("\"cash\":\"2.05\""), "{out}");
    }

    /// An invoice-with-tax flow: percentages round to the currency precision, `$`/`money` alias.
    #[test]
    fn money_invoice_with_tax() {
        let out = run_script(
            "function handler() { \
               const net = $('100.00', 'USD'); \
               const gross = net.add_pct(8.25); \
               return json({ gross: gross.to_string(), alias: money === $, \
                 minor: gross.to_minor(), fmt: gross.format() }); }",
            None,
        );
        assert!(out.contains("\"gross\":\"108.25\""), "{out}");
        assert!(out.contains("\"alias\":true"), "{out}");
        assert!(out.contains("\"minor\":10825"), "{out}");
        assert!(out.contains("\"fmt\":\"$108.25\""), "{out}");
    }

    /// A refund split: penny-safe allocation whose shares sum to the total exactly.
    #[test]
    fn money_refund_split_is_penny_safe() {
        let out = run_script(
            "function handler() { \
               const parts = $('100.00', 'USD').allocate_to(3).map(function (m) { return m.to_string(); }); \
               return json({ parts: parts }); }",
            None,
        );
        assert!(
            out.contains("[\"33.34\",\"33.33\",\"33.33\"]"),
            "equal split sums to 100.00: {out}"
        );
    }

    /// `div` is overloaded: money ÷ scalar → money, money ÷ same-currency money → a Decimal ratio.
    #[test]
    fn money_div_overload() {
        let out = run_script(
            "function handler() { \
               const unit = $('99.00', 'USD').div(3).to_string(); \
               const ratio = $('115.00', 'USD').div($('100.00', 'USD')).toString(); \
               return json({ unit: unit, ratio: ratio }); }",
            None,
        );
        // Money arithmetic stays exact (rounding is explicit): 99.00 / 3 keeps the 2-place scale.
        assert!(out.contains("\"unit\":\"33.00\""), "{out}");
        assert!(out.contains("\"ratio\":\"1.15\""), "money/money ratio: {out}");
    }

    /// Currency safety: cross-currency add throws a catchable error (no implicit FX).
    #[test]
    fn money_cross_currency_add_throws() {
        let out = run_script(
            "function handler() { \
               try { $('1.00','USD').add($('1.00','EUR')); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
            None,
        );
        assert!(out.contains("\"caught\":true"), "{out}");
    }

    /// The currency cascade: no explicit arg falls back to the resolved default currency.
    #[test]
    fn money_currency_cascade_uses_default() {
        let out = run_script(
            "function handler() { return json($('19.99')); }",
            Some("EUR"),
        );
        assert!(out.contains("\"currency\":\"EUR\""), "{out}");
        assert!(out.contains("\"minor_units\":1999"), "{out}");
    }

    /// No currency resolvable → a catchable construction error.
    #[test]
    fn money_no_currency_throws() {
        let out = run_script(
            "function handler() { \
               try { $('19.99'); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
            None,
        );
        assert!(out.contains("\"caught\":true"), "{out}");
    }

    /// Self-describing serialization: JPY (exponent 0) has integer `minor_units`, no ×100.
    #[test]
    fn money_serializes_self_describing_zero_decimal() {
        let out = run_script(
            "function handler() { return json({ y: $('1000', 'JPY') }); }",
            None,
        );
        assert!(out.contains("\"amount\":\"1000\""), "{out}");
        assert!(out.contains("\"currency\":\"JPY\""), "{out}");
        assert!(out.contains("\"minor_units\":1000"), "not 100000: {out}");
    }

    /// The ISO 4217 exponent table drives precision: BHD=3, CLF=4 minor units; an unknown code throws.
    #[test]
    fn money_currency_exponent_table() {
        let bhd = run_script("function handler() { return json($('1.234', 'BHD')); }", None);
        assert!(bhd.contains("\"minor_units\":1234"), "BHD exponent 3: {bhd}");
        let clf = run_script("function handler() { return json($('1.2345', 'CLF')); }", None);
        assert!(clf.contains("\"minor_units\":12345"), "CLF exponent 4: {clf}");
        let bad = run_script(
            "function handler() { \
               try { $('10', 'ZZZ'); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
            None,
        );
        assert!(bad.contains("\"caught\":true"), "unknown currency throws: {bad}");
    }
}

/// The capability mux (`io.call`) + registry: a wired egress/registry exposes `io.call`, success
/// JSON flows back, a `EgressError` round-trips as a classified capability error, and the seam is
/// withheld under `Profile::Deterministic`. Gated to the capability-free build so `ExecParams`
/// has no in-engine (`http`/`s3`) fields to populate.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod egress_tests {
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
        fn call(
            &self,
            _name: &str,
            action: &str,
            payload_json: &str,
        ) -> Result<String, EgressError> {
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
        fn call(
            &self,
            _name: &str,
            _action: &str,
            _payload_json: &str,
        ) -> Result<String, EgressError> {
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
            "function handler(ctx) { return json(io.call('orders', 'ping', { x: ctx.n })); }";
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
        let script = "function handler(ctx) { return json(io.call('orders', 'fail', {})); }";
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
        let script = "function handler() { return json(typeof io); }";
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
            "globalThis.widget = { ping: function () { return io.call('widget', 'ping', {}); } };";
        let def = CapabilityDef::new("widget", wrapper, "", Trust::OperatorSupplied);
        let egress: Arc<dyn Egress> = Arc::new(EchoEgress);
        let reg = CapabilityRegistry::build(vec![def], None).unwrap_or_else(|_err| unreachable!());
        let script = "function handler() { return json({ widget: typeof widget, missing: typeof gadget }); }";
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
            try { io.call('secret', 'query', {}); return json('no throw'); } \
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
            "function handler() { return json(io.call('db', 'get', { host: '10.0.0.1' })); }";
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
            "function handler() { return json(io.call('db', 'get', { host: '93.184.216.34' })); }";
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
        let script = "function handler() { return json(io.call('db', 'get', {})); }";
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
        let script = "function handler() { return json(io.call('db', 'get', {})); }";
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
        let orders =
            CapabilityDef::new("orders", "", "", Trust::OperatorSupplied).with_backend(local);
        let amq = CapabilityDef::new("amq", "", "", Trust::OperatorSupplied);
        let reg = CapabilityRegistry::build(vec![orders, amq], None)
            .unwrap_or_else(|_err| unreachable!());
        let fallback: Arc<dyn Egress> = Arc::new(EchoEgress);
        let script = "function handler() { return json({ \
            a: io.call('orders', 'ping', {}), \
            b: io.call('amq', 'publish', { m: 1 }) }); }";
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
            try { io.call('db', 'query', {}); return json('no throw'); } \
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
}

/// `emit(kind, value)` tagged effects channel — behavioral coverage for the capture, ordering,
/// validation, and bounds. Compiled in every build (`emit` is injected regardless of `_io`), so
/// the `ExecParams` builder mirrors the in-engine cfg-gated fields.
#[cfg(test)]
mod emit_tests {
    use super::{ExecOutcome, ExecParams, ExecResult, Profile, run};
    use rquickjs::Runtime;
    use std::time::Duration;

    /// Runs `script` under the full profile with an 8-op cap and a 64-char `kind` bound, and
    /// returns the full [`ExecResult`] (a handler throw is a `Success`/`Error` outcome, not an
    /// engine error).
    fn run_exec(script: &str) -> ExecResult {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let params = ExecParams {
            runtime: &runtime,
            bytecode_cache: None,
            cache_namespace: None,
            script,
            context_json: "{}",
            timeout: Duration::from_secs(5),
            profile: Profile::Full,
            #[cfg(feature = "http")]
            allowed_hosts: &[],
            #[cfg(feature = "s3")]
            s3_config: None,
            sys_config: None,
            registry: None,
            enabled_io: &[],
            egress: None,
            default_currency: None,
            max_ops: 8,
            max_emit_kind_len: 64,
            log_level: super::LogLevel::Info,
            max_log_entries: 256,
            max_log_entry_bytes: 256 * 1024,
            max_log_total_bytes: 1024 * 1024,
            max_output_size: 0,
            allow_private_targets: false,
            #[cfg(feature = "http")]
            wildcard_hosts_allowed: false,
        };
        run(&params).unwrap_or_else(|_err| unreachable!("engine ran"))
    }

    /// Multiple emits are captured in call order, preserving duplicate kinds, as tagged
    /// `{kind, value}` entries.
    #[test]
    fn emit_preserves_order_and_duplicate_kinds() {
        let exec = run_exec(
            "function handler() { emit(\"a\", 1); emit(\"b\", 2); emit(\"a\", 3); return json(1); }",
        );
        let tags: Vec<&str> = exec
            .effects
            .iter()
            .map(|effect| effect.kind.as_str())
            .collect();
        assert_eq!(tags, ["a", "b", "a"], "kinds preserved in call order");
        let vals: Vec<&str> = exec
            .effects
            .iter()
            .map(|effect| effect.value.get())
            .collect();
        assert_eq!(vals, ["1", "2", "3"], "values preserved in call order");
    }

    /// An empty `kind` fails deterministically and records nothing.
    #[test]
    fn emit_with_empty_kind_fails_and_records_nothing() {
        let exec = run_exec("function handler() { emit(\"\", 5); return json(1); }");
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(_)),
            "empty kind throws"
        );
        assert!(
            exec.effects.is_empty(),
            "no effect recorded on the failed emit"
        );
    }

    /// A single-arg `emit(value)` (no kind) fails deterministically and records nothing.
    #[test]
    fn emit_with_missing_kind_fails_and_records_nothing() {
        let exec = run_exec("function handler() { emit(5); return json(1); }");
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(_)),
            "missing kind throws"
        );
        assert!(exec.effects.is_empty(), "no effect recorded");
    }

    /// Effects emitted before a handler throws are still captured (capture-on-failure).
    #[test]
    fn emit_survives_a_handler_throw() {
        let exec = run_exec(
            "function handler() { emit(\"finding\", { id: 7 }); throw new Error(\"boom\"); }",
        );
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(_)),
            "handler threw"
        );
        let kinds: Vec<&str> = exec
            .effects
            .iter()
            .map(|effect| effect.kind.as_str())
            .collect();
        let vals: Vec<&str> = exec
            .effects
            .iter()
            .map(|effect| effect.value.get())
            .collect();
        assert_eq!(kinds, ["finding"], "the pre-throw effect kind is retained");
        assert_eq!(
            vals,
            ["{\"id\":7}"],
            "the pre-throw effect value is retained"
        );
    }

    /// Exceeding the per-execution emit cap (`max_ops`, 8 here) fails the over-limit call; the
    /// buffer stops growing at the cap.
    #[test]
    fn emit_beyond_the_cap_fails() {
        let exec = run_exec(
            "function handler() { for (var i = 0; i < 9; i++) { emit(\"k\", i); } return json(1); }",
        );
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(_)),
            "the 9th emit (cap is 8) throws"
        );
        assert_eq!(exec.effects.len(), 8, "the buffer stops at the cap");
    }

    /// A `kind` longer than `max_emit_kind_len` (64 here) is rejected and records nothing.
    #[test]
    fn emit_with_overlong_kind_fails() {
        let exec = run_exec("function handler() { emit(\"x\".repeat(65), 1); return json(1); }");
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(_)),
            "an over-length kind throws"
        );
        assert!(exec.effects.is_empty(), "no effect recorded");
    }
}

/// `log.*` diagnostic channel — structured capture, the level floor, bound context, the D7 bounds,
/// capture-on-failure, and the determinism/timing split. Compiled in every build (`log` is injected
/// regardless of `_io`), so the `ExecParams` builder mirrors the cfg-gated in-engine fields.
#[cfg(test)]
mod log_tests {
    use super::{ExecOutcome, ExecParams, ExecResult, LogLevel, Profile, run};
    use rquickjs::Runtime;
    use std::time::Duration;

    /// Small per-execution log bounds for the bound tests (5 entries, 200 B/entry, 500 B total).
    struct LogCaps {
        /// Level floor.
        floor: LogLevel,
        /// Count cap.
        entries: usize,
        /// Per-entry byte cap.
        entry_bytes: usize,
        /// Total byte cap.
        total_bytes: usize,
    }

    impl LogCaps {
        /// Production-like caps at the `info` floor.
        const fn info() -> Self {
            Self {
                floor: LogLevel::Info,
                entries: 256,
                entry_bytes: 256 * 1024,
                total_bytes: 1024 * 1024,
            }
        }
    }

    /// Runs `script` under `profile` with the given caps, returning the full [`ExecResult`].
    fn run_logs(script: &str, profile: Profile, caps: &LogCaps) -> ExecResult {
        let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
        let params = ExecParams {
            runtime: &runtime,
            bytecode_cache: None,
            cache_namespace: None,
            script,
            context_json: "{}",
            timeout: Duration::from_secs(5),
            profile,
            #[cfg(feature = "http")]
            allowed_hosts: &[],
            #[cfg(feature = "s3")]
            s3_config: None,
            sys_config: None,
            registry: None,
            enabled_io: &[],
            egress: None,
            default_currency: None,
            max_ops: 64,
            max_emit_kind_len: 64,
            log_level: caps.floor,
            max_log_entries: caps.entries,
            max_log_entry_bytes: caps.entry_bytes,
            max_log_total_bytes: caps.total_bytes,
            max_output_size: 0,
            allow_private_targets: false,
            #[cfg(feature = "http")]
            wildcard_hosts_allowed: false,
        };
        run(&params).unwrap_or_else(|_err| unreachable!("engine ran"))
    }

    /// 4.1 — `log.info` records a structured entry (level, template, properties, rendered message,
    /// seq); order is preserved with strictly increasing `seq`.
    #[test]
    fn records_structured_entries_in_order() {
        let exec = run_logs(
            "function handler() { \
               log.info(\"charged {user} {amount}\", { user: 42, amount: \"10.00\" }); \
               log.warn(\"second\"); \
               log.error(\"third\"); \
               return json(1); }",
            Profile::Full,
            &LogCaps::info(),
        );
        assert_eq!(exec.logs.len(), 3, "three entries recorded");
        let first = &exec.logs[0];
        assert_eq!(first.level, LogLevel::Info);
        assert_eq!(first.template, "charged {user} {amount}");
        assert_eq!(first.message, "charged 42 10.00", "template rendered");
        assert!(
            first.properties.get().contains("\"user\":42")
                && first.properties.get().contains("\"amount\":\"10.00\""),
            "properties carried: {}",
            first.properties.get()
        );
        assert_eq!(
            [exec.logs[0].seq, exec.logs[1].seq, exec.logs[2].seq],
            [0, 1, 2],
            "seq is strictly increasing in call order"
        );
    }

    /// 4.2 — a below-floor `log.debug` records nothing (floor is `info`).
    #[test]
    fn below_floor_records_nothing() {
        let exec = run_logs(
            "function handler() { log.debug(\"x\", { a: 1 }); log.trace(\"y\"); return json(1); }",
            Profile::Full,
            &LogCaps::info(),
        );
        assert!(exec.logs.is_empty(), "below-floor calls record no entries");
    }

    /// An at-or-above-floor call is recorded.
    #[test]
    fn at_floor_records() {
        let exec = run_logs(
            "function handler() { log.warn(\"careful\"); return json(1); }",
            Profile::Full,
            &LogCaps::info(),
        );
        assert_eq!(exec.logs.len(), 1);
        assert_eq!(exec.logs[0].level, LogLevel::Warn);
    }

    /// 4.3 — `log.with(ctx)` merges bound context into subsequent entries, with per-call keys
    /// overriding the bound value (OQ3).
    #[test]
    fn bound_context_merges_and_call_overrides() {
        let exec = run_logs(
            "function handler() { \
               var l = log.with({ order: 7, tier: \"a\" }); \
               l.info(\"done\", { ok: true, tier: \"b\" }); \
               return json(1); }",
            Profile::Full,
            &LogCaps::info(),
        );
        assert_eq!(exec.logs.len(), 1);
        let props = exec.logs[0].properties.get();
        assert!(
            props.contains("\"order\":7"),
            "bound field present: {props}"
        );
        assert!(
            props.contains("\"ok\":true"),
            "per-call field present: {props}"
        );
        assert!(
            props.contains("\"tier\":\"b\""),
            "per-call key overrides bound: {props}"
        );
    }

    /// 4.4a — exceeding the per-execution count cap drops further entries (execution unaffected).
    #[test]
    fn count_cap_drops_further_entries() {
        let caps = LogCaps {
            entries: 3,
            ..LogCaps::info()
        };
        let exec = run_logs(
            "function handler() { for (var i = 0; i < 10; i++) log.info(\"n\", { i: i }); \
               return json(\"ok\"); }",
            Profile::Full,
            &caps,
        );
        assert_eq!(exec.logs.len(), 3, "capped at the count limit");
        assert!(
            matches!(exec.outcome, ExecOutcome::Success(_)),
            "over-cap logging does not fail the run"
        );
    }

    /// 4.4b — an oversize entry is truncated with a `{"truncated":true}` marker rather than dropped.
    #[test]
    fn oversize_entry_is_truncated() {
        let caps = LogCaps {
            entry_bytes: 128,
            ..LogCaps::info()
        };
        let exec = run_logs(
            "function handler() { log.info(\"big {blob}\", { blob: \"y\".repeat(500) }); \
               return json(1); }",
            Profile::Full,
            &caps,
        );
        assert_eq!(exec.logs.len(), 1, "the oversize entry is kept (truncated)");
        assert!(
            exec.logs[0].properties.get().contains("truncated"),
            "carries the truncation marker: {}",
            exec.logs[0].properties.get()
        );
    }

    /// 4.5 — a handler that logs then throws still carries the entries (capture-on-failure).
    #[test]
    fn logs_survive_a_handler_throw() {
        let exec = run_logs(
            "function handler() { log.info(\"step 1\"); throw new Error(\"boom\"); }",
            Profile::Full,
            &LogCaps::info(),
        );
        assert!(
            matches!(exec.outcome, ExecOutcome::Error(_)),
            "the handler threw"
        );
        assert_eq!(exec.logs.len(), 1, "the pre-throw entry is retained");
        assert_eq!(exec.logs[0].message, "step 1");
    }

    /// 4.6 — a deterministic run carries `seq` but no timing; a full-profile run may carry
    /// `offset_us`; `data` is byte-identical across two deterministic runs regardless of logging.
    #[test]
    fn determinism_seq_without_timing() {
        let script = "function handler() { log.info(\"tick\"); return json({ n: 1 }); }";
        let det_a = run_logs(script, Profile::Deterministic, &LogCaps::info());
        let det_b = run_logs(script, Profile::Deterministic, &LogCaps::info());
        assert_eq!(det_a.logs.len(), 1);
        assert_eq!(det_a.logs[0].seq, 0, "seq present under determinism");
        assert!(
            det_a.logs[0].offset_us.is_none(),
            "no wall-clock timing under the deterministic profile"
        );
        let (ExecOutcome::Success(a), ExecOutcome::Success(b)) = (&det_a.outcome, &det_b.outcome)
        else {
            unreachable!("both deterministic runs succeed");
        };
        assert_eq!(a, b, "data is byte-identical across deterministic runs");

        let full = run_logs(script, Profile::Full, &LogCaps::info());
        assert!(
            full.logs[0].offset_us.is_some(),
            "the full profile attaches a relative timing offset"
        );
    }
}
