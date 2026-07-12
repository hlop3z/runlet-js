//! The `emit` effects channel, the `log.*` diagnostic channel, and the capability mux.
//!
//! Injects the host functions a handler calls to surface data (`emit`), diagnostics (`log.*`), and
//! egress (`io.call` via the mux), plus the per-invocation buffers they append to and the drains
//! that turn those buffers into [`Effect`]s / [`LogEntry`]s after execution (both paths — capture
//! on failure). The core meters and bounds these but never interprets their payloads.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rquickjs::{Ctx, Function, Value as JsValue};
use serde_json::value::RawValue;

use crate::capability::{CapabilityRegistry, MuxCall};
use crate::errors::{self, ErrorOwner};

use super::types::{Effect, ExecParams, LogEntry, LogLevel};

/// The generic `io.call` egress wrapper — loaded from `src/js/io.js` at compile
/// time. `eval`'d after `__io` is registered, only when a [`Egress`] is wired and the
/// profile is `Full` (the seam is I/O).
const IO_WRAPPER: &str = include_str!("../js/io.js");

/// The `log.*` diagnostic wrapper — loaded from `src/js/log.js` at compile time. `eval`'d after
/// `__log` + `__logFloor` are registered; injected under both profiles (D8: a deterministic script
/// is often exactly the one you want to debug), so it sits beside `inject_emit` in `run`.
const LOG_WRAPPER: &str = include_str!("../js/log.js");

/// The per-invocation `emit` buffer: native `__emit` appends `(kind, value_json)` pairs, drained
/// into [`ExecResult::effects`] after execution.
pub(super) type EmitBuffer = Arc<Mutex<Vec<(String, String)>>>;

/// One captured `(level, template, properties_json, message, seq, offset_us)` tuple, before it is
/// drained into a public [`LogEntry`]. Held in the [`LogBuffer`]'s accumulator.
pub(super) struct RawLog {
    /// Severity level.
    pub(super) level: LogLevel,
    /// The message template.
    pub(super) template: String,
    /// The merged properties as a JSON string (verbatim from the wrapper's `JSON.stringify`).
    pub(super) properties_json: String,
    /// The JS-rendered message.
    pub(super) message: String,
    /// Call-order sequence number.
    pub(super) seq: u64,
    /// Relative microseconds from execution start (`None` under the deterministic profile).
    pub(super) offset_us: Option<u128>,
}

/// The per-invocation `log` accumulator: the captured entries plus a running byte total, so the D7
/// per-execution total bound (`max_log_total_bytes`) can be enforced across entries.
#[derive(Default)]
pub(super) struct LogAccumulator {
    /// Captured entries, in call order.
    pub(super) entries: Vec<RawLog>,
    /// Running sum of the captured entries' sizes (template + properties + message bytes).
    pub(super) total_bytes: usize,
}

/// The shared per-invocation `log` buffer: native `__log` appends [`RawLog`]s here (bounded by the
/// D7 triad), drained into [`ExecResult::logs`] after execution on both paths (capture-on-failure).
pub(super) type LogBuffer = Arc<Mutex<LogAccumulator>>;

/// One candidate log entry crossing from the native `__log` into [`capture_log`], before the D7
/// bounds decide to keep, truncate, or drop it. Bundled so the capture helper stays within the
/// argument-count lint.
pub(super) struct PendingLog {
    /// Severity level.
    pub(super) level: LogLevel,
    /// The message template.
    pub(super) template: String,
    /// The merged properties as a JSON string.
    pub(super) properties_json: String,
    /// The JS-rendered message.
    pub(super) message: String,
    /// Relative microseconds from execution start (`None` under the deterministic profile).
    pub(super) offset_us: Option<u128>,
}

/// The D7 per-execution log bounds + the resolved level floor, threaded into the native `__log`.
#[derive(Clone, Copy)]
pub(super) struct LogLimits {
    /// The resolved minimum level (below it, the wrapper records nothing).
    pub(super) floor: LogLevel,
    /// Max entries per execution; beyond it further calls are dropped.
    pub(super) max_entries: usize,
    /// Max bytes per entry; an oversize entry is truncated with a marker.
    pub(super) max_entry_bytes: usize,
    /// Max total bytes per execution (binds first); a call that would exceed it is dropped.
    pub(super) max_total_bytes: usize,
}

/// Injects the `emit(kind, value)` host function: it `JSON.stringify`s the value and appends a
/// `(kind, value_json)` pair to the per-invocation `effects` buffer (surfaced as
/// `Outcome.effects`). `kind` is a required non-empty string tag bounded by `max_emit_kind_len`
/// (an over-long or empty tag is rejected and records nothing); the number of effects is capped
/// at `max_ops` so a handler can't grow the buffer without bound. The `value` is opaque to the
/// core — the consumer interprets it ("logic proposes, the engine disposes"); the `kind` is a
/// routing/governance tag the core surfaces but never interprets.
pub(super) fn inject_emit(
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
    // is an error the wrapper throws (the native side re-checks non-empty + the length bound). It is
    // defined on `$std` (like every built-in) and mirrored to the `emit` global by the projection.
    let wrapper: JsValue<'_> = qctx.eval(
        "$std.emit = function (kind, value) { \
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
pub(super) fn inject_log(
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
pub(super) fn drain_logs(logs: &LogBuffer) -> Vec<LogEntry> {
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
pub(super) fn inject_registry(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
) -> Result<(), rquickjs::Error> {
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
pub(super) fn drain_effects(effects: &EmitBuffer) -> Vec<Effect> {
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
