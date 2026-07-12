//! Handler resolution, module evaluation, and error classification.
//!
//! After `$std` and the capabilities are injected, this module evaluates the user source (classic
//! script or ES module, with the bytecode-cache fast path), hardens the environment (`eval`/`Proxy`
//! removal + the determinism prune), resolves the `handler` function, and invokes it. On failure it
//! **classifies** the outcome structurally — timeout flag → `__runlet` capability tag → script
//! error — and assembles the typed [`EngineError`]/[`CapabilityErr`] into a public `ErrorEnvelope`.

use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rquickjs::module::{Declared, Evaluated};
use rquickjs::{Ctx, Function, Module, Object, Value as JsValue, WriteOptions};
use serde::Deserialize;
use serde_json::Value;

use crate::bytecode;
use crate::errors::{ErrorCategory, ErrorDebug, ErrorEnvelope, ErrorOwner, ErrorSource};
use crate::modules;

use super::types::{CapabilityErr, EngineError, ExecOutcome, ExecParams, Profile};

/// Human-safe message for a missing `handler`.
const HANDLER_MISSING_MSG: &str = "script must define a `handler(context)` function";
/// Human-safe message for an out-of-memory abort.
const MEMORY_MSG: &str = "memory limit exceeded";

/// Determinism sanitizer — loaded from `src/js/determinism.js` at compile time. Run after
/// `sanitize_globals` under [`Profile::Deterministic`] to neutralize the nondeterministic JS
/// **builtins** that are not `$std` members (`Math.random`, `Date.now`, zero-arg `new Date()`). The
/// prunable `$std` members (`$std.datetime.now`, `$std.crypto.uuid`) are removed in the lazy builder
/// instead (see `build_unit_sources`), so this pass never reads — and thus never force-builds — a
/// lazy `$std` member.
const DETERMINISM_SANITIZER: &str = include_str!("../js/determinism.js");

/// The `__runlet` tag deserialized from a thrown capability error (read in one
/// `json_stringify` + parse rather than field-by-field).
#[derive(Debug, Deserialize)]
pub(super) struct CapabilityTag {
    /// Raw driver cause.
    #[serde(default)]
    pub(super) error: Option<String>,
    /// Stable machine code.
    pub(super) code: String,
    /// Retry hint.
    #[serde(default)]
    pub(super) retryable: bool,
    /// Originating capability (lowercase, parsed via [`ErrorSource::parse`]).
    pub(super) source: String,
    /// Responsible owner (lowercase, parsed via [`ErrorOwner::parse`]).
    #[serde(default)]
    pub(super) owner: Option<String>,
    /// Structured machine context.
    #[serde(default)]
    pub(super) details: Option<Value>,
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
/// `Proxy` to deny exotic-object traps over the injected `$std` surface. Do not
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
pub(super) fn resolve_handler<'js>(
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

/// Neutralizes the nondeterministic JS builtins for [`Profile::Deterministic`]: removes
/// `Math.random`, `Date.now`, and zero-arg `new Date()` (see `js/determinism.js`). The prunable
/// `$std` members (`$std.datetime.now`, `$std.crypto.uuid`) are pruned in their lazy builder, not
/// here. Runs after [`sanitize_globals`].
fn sanitize_determinism(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let sanitized: JsValue<'_> = qctx.eval(DETERMINISM_SANITIZER)?;
    drop(sanitized);
    Ok(())
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
pub(super) fn invoke_handler<'js>(
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
    pub(super) fn internal<E: Display>(err: E) -> Self {
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
