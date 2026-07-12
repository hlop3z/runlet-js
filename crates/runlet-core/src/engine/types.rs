//! Engine data types.
//!
//! Pure definitions shared across the engine's submodules: the capability/determinism
//! [`Profile`], the [`Gate`] presence flag, the declarative [`Effect`] and diagnostic
//! [`LogEntry`]/[`LogLevel`] records, the [`ExecParams`] input and [`ExecResult`] output, the
//! success/error [`ExecOutcome`], and the classified [`EngineError`]/[`CapabilityErr`]. No engine
//! logic lives here — the injection, channel, and classification modules operate on these.

use std::sync::Arc;
use std::time::Duration;

use rquickjs::Runtime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::PrecompiledSurface;
use crate::bytecode::BytecodeCache;
use crate::capability::CapabilityRegistry;
use crate::egress::Egress;
use crate::errors::{ErrorOwner, ErrorSource};
#[cfg(feature = "http")]
use crate::http::HttpMetric;
#[cfg(feature = "s3")]
use crate::s3::{S3Config, S3Metric};
use crate::sys::SysConfig;

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
    /// `eval`/`Proxy` removal. Only the pure `$std` helpers (`$`, crypto, …), `emit`, and a
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

/// Parameters for a single script execution. Built by the [`crate::host::LogicHost`] from
/// a public `Invocation`; internal to the core.
pub(crate) struct ExecParams<'a> {
    /// The pooled runtime.
    pub(crate) runtime: &'a Runtime,
    /// Shared compiled-bytecode cache (parse/compile reuse for the ES-module path).
    /// `None` = always recompile (e.g. a consumer that opts out).
    pub(crate) bytecode_cache: Option<&'a BytecodeCache>,
    /// Precompiled injected-framework-surface bytecode, produced once at pool warm-up and loaded
    /// per request instead of re-parsing the framework JS. `None` = parse the framework source
    /// each request (the fallback for a consumer/pool that builds no surface, and for tests).
    pub(crate) surface: Option<&'a PrecompiledSurface>,
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
    /// `$std` env/secrets context (None = no env/secrets injected).
    pub(crate) sys_config: Option<&'a SysConfig>,
    /// The composed capability registry (the mux's per-name routing table + the JS wrappers to
    /// inject). `None` = no registered capabilities. Under [`Profile::Deterministic`] the mux and
    /// every wrapper are withheld (they perform I/O), regardless of registration.
    pub(crate) registry: Option<&'a CapabilityRegistry>,
    /// Names of registered egress capabilities to enable for this request (per-request, opt-in).
    /// A registered def's wrapper is injected only if its name appears here.
    pub(crate) enabled_io: &'a [&'a str],
    /// Per-request fallback egress for the mux (the broker). Consulted for any name
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
    pub(super) source: ErrorSource,
    /// Stable machine code (set in Rust, round-tripped through the tag).
    pub(super) code: String,
    /// Retry hint.
    pub(super) retryable: bool,
    /// Who should act on the error.
    pub(super) owner: ErrorOwner,
    /// Raw driver cause — surfaced gated, in `debug.raw`.
    pub(super) raw: Option<String>,
    /// JS stack trace, when available.
    pub(super) stack: Option<String>,
    /// Structured, ungated machine context (e.g. `{sqlstate}` / `{http_status}`).
    pub(super) details: Option<Value>,
}
