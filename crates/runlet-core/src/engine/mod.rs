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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Runtime};

#[cfg(feature = "http")]
use crate::http::HttpMetric;
#[cfg(feature = "s3")]
use crate::s3::S3Metric;
#[cfg(any(feature = "http", feature = "s3"))]
use crate::sandbox::{self, Collector};

mod channels;
mod classify;
mod inject;
mod types;

pub(crate) use inject::{PrecompiledSurface, compile_surface};
pub use types::{
    CapabilityErr, Effect, EngineError, ExecOutcome, Gate, LogEntry, LogLevel, Profile,
};
pub(crate) use types::{ExecParams, ExecResult};

use channels::{
    EmitBuffer, LogAccumulator, LogBuffer, LogLimits, drain_effects, drain_logs, inject_emit,
    inject_log, inject_registry,
};
use classify::{invoke_handler, resolve_handler};
#[cfg(feature = "_io")]
use inject::inject_apis;
use inject::{
    freeze_std, inject_bridge, inject_lazy_std, inject_std_bootstrap, project_std_globals,
};

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
        // D3 step 1 — bootstrap `$std` before any member is built onto it.
        inject_std_bootstrap(&qctx, params.surface).map_err(EngineError::internal)?;
        inject_bridge(&qctx, params.surface).map_err(EngineError::internal)?;
        // D1/D2 — register the cheap eager native FFI bridges up front, then install the value-util
        // members as lazy getter-only accessors. A member's (expensive) wrapper IIFE is parsed +
        // executed only on first access within the request; an untouched member is never built. The
        // deterministic prune of `$std.datetime.now`/`$std.crypto.uuid` is folded into the lazy
        // builder (D4), so it never force-builds an untouched member.
        inject_lazy_std(&qctx, params).map_err(EngineError::internal)?;
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
        // D3 step 3 — project the curated `$std` members onto `globalThis` before the user script
        // evals, so it sees `$`/`json`/`log`/`emit`. `resolve_handler` then evals + hardens (D3
        // steps 4–6: sanitize `eval`/`Proxy`, and under the deterministic profile prune the ambient
        // authorities).
        project_std_globals(&qctx, params.surface).map_err(EngineError::internal)?;
        let handler = match resolve_handler(&qctx, params) {
            Ok(func) => func,
            Err(outcome) => return Ok(outcome),
        };
        // D3 step 7 — deep-freeze `$std` and lock the projected globals, strictly after the prune
        // and before `handler` runs, so the surface is tamper-proof for the invocation.
        freeze_std(&qctx, params.surface).map_err(EngineError::internal)?;
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

#[cfg(test)]
mod tests;

/// The bytecode cache must populate on the first run and produce a byte-identical result on
/// the second (the `Module::load` path), and must autonomously skip caching sources below its
/// size floor. The extra `cfg` gates to the capability-free build so `ExecParams` has no I/O
/// fields to populate; the separate `#[cfg(test)]` keeps `tests_outside_test_module` satisfied.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod bytecode_cache_tests;

/// End-to-end `Decimal` (exact number) + `$` / `money` (currency-bound) surface: drives the JS
/// wrappers (`decimal.js` + `money.js`) through the engine so the FFI, the ISO 4217 table, the
/// currency cascade, and serialization are all exercised together. Capability-free build (no
/// in-engine `http`/`s3` fields on `ExecParams`).
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod money_tests;

/// End-to-end `datetime` value-util: drives the JS wrapper (`datetime.js`) + the Rust `datetime`
/// domain (`sys.rs`, chrono + chrono-tz) through the engine so parsing, components, calendar
/// arithmetic, boundaries, business-day helpers, diff, timezone views, formatting, serialization,
/// and the deterministic-profile clock removal are all exercised together. Capability-free build
/// (no in-engine `http`/`s3` fields on `ExecParams`).
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod datetime_tests;

/// End-to-end `template` value-util: drives the JS wrapper (`template.js`) + the Rust `minijinja`
/// bridge (`template.rs`) through the engine so the two escaping modes, lenient-undefined +
/// placeholder, merge-tag introspection, eager syntax-error surfacing, and the both-profile
/// determinism guarantee are all exercised together. Capability-free build (no in-engine `http`/`s3`
/// fields on `ExecParams`).
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod template_tests;

/// The `$std` canonical namespace + its `globalThis` projection: every built-in is reachable
/// through `$std`, the curated globals are identity-equal mirrors, the former bare util globals are
/// gone, prunable authorities are unreachable via every path under the deterministic profile, and
/// the surface is frozen/locked before the handler runs. Capability-free build (no `http`/`s3`/`io`
/// fields on `ExecParams`), so this exercises the always-on utils + `$std.crypto`.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod std_namespace_tests;

/// The capability mux (`io.call`) + registry: a wired egress/registry exposes `io.call`, success
/// JSON flows back, a `EgressError` round-trips as a classified capability error, and the seam is
/// withheld under `Profile::Deterministic`. Gated to the capability-free build so `ExecParams`
/// has no in-engine (`http`/`s3`) fields to populate.
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod egress_tests;

/// `emit(kind, value)` tagged effects channel — behavioral coverage for the capture, ordering,
/// validation, and bounds. Compiled in every build (`emit` is injected regardless of `_io`), so
/// the `ExecParams` builder mirrors the in-engine cfg-gated fields.
#[cfg(test)]
mod emit_tests;

/// `log.*` diagnostic channel — structured capture, the level floor, bound context, the D7 bounds,
/// capture-on-failure, and the determinism/timing split. Compiled in every build (`log` is injected
/// regardless of `_io`), so the `ExecParams` builder mirrors the cfg-gated in-engine fields.
#[cfg(test)]
mod log_tests;

#[cfg(test)]
mod lazy_std_tests;

/// Golden equivalence: the injected framework surface loaded from precompiled bytecode is
/// behaviorally identical to the same surface parsed from source (globals, projection identity,
/// freeze/lock, determinism prune) under both profiles, and bytecode reuse never leaks state
/// across requests. Capability-free build (no `http`/`s3`/`io` fields on `ExecParams`).
#[cfg(test)]
#[cfg(not(feature = "_io"))]
mod surface_bytecode_tests;
