//! Golden equivalence for the precompiled injected surface (tasks.md 3.3).
//!
//! A context built by loading the framework surface from precompiled **bytecode** must be
//! behaviorally identical to one built by parsing the framework **source** — the classic-script →
//! module conversion must preserve every observable of the injected surface (globals, projection
//! identity, deep-freeze/lock, the determinism prune) under BOTH profiles. The `surface: None`
//! path parses source; `surface: Some(compiled)` loads bytecode. Every assertion runs the SAME
//! script through both and requires byte-identical output, so a divergence — even an error — fails
//! the test.
//!
//! Also asserts the modified per-request-isolation requirement: bytecode reuse restores compiled
//! *code*, never a prior request's *state*, so a global/prototype mutation never leaks forward.

use super::{ExecOutcome, ExecParams, PrecompiledSurface, Profile, compile_surface, run};
use rquickjs::Runtime;
use std::time::Duration;

/// A full-profile `ExecParams` over a compiled surface for the given `script` — a single lifetime
/// `'a` ties the runtime, surface, and script together (an inline closure cannot express this).
fn surface_params<'a>(
    runtime: &'a Runtime,
    surface: &'a PrecompiledSurface,
    script: &'a str,
) -> ExecParams<'a> {
    ExecParams {
        runtime,
        bytecode_cache: None,
        surface: Some(surface),
        cache_namespace: None,
        script,
        context_json: "{}",
        timeout: Duration::from_secs(5),
        profile: Profile::Full,
        sys_config: None,
        registry: None,
        enabled_io: &[],
        egress: None,
        default_currency: Some("USD"),
        max_ops: 4096,
        max_emit_kind_len: 64,
        log_level: super::LogLevel::Info,
        max_log_entries: 256,
        max_log_entry_bytes: 256 * 1024,
        max_log_total_bytes: 1024 * 1024,
        max_output_size: 0,
        allow_private_targets: false,
    }
}

/// Runs `script` under `profile`, building the injected surface from precompiled bytecode when
/// `use_surface`, else from parsed source (the fallback). Returns the `data`/`error` envelope JSON.
fn run_variant(script: &str, profile: Profile, use_surface: bool) -> String {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let surface =
        use_surface.then(|| compile_surface(&runtime).unwrap_or_else(|_err| unreachable!()));
    let params = ExecParams {
        runtime: &runtime,
        bytecode_cache: None,
        surface: surface.as_ref(),
        cache_namespace: None,
        script,
        context_json: "{}",
        timeout: Duration::from_secs(5),
        profile,
        sys_config: None,
        registry: None,
        enabled_io: &[],
        egress: None,
        default_currency: Some("USD"),
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

/// Asserts the source-eval and bytecode-load paths produce byte-identical output.
fn assert_equivalent(script: &str, profile: Profile) {
    let from_source = run_variant(script, profile, false);
    let from_bytecode = run_variant(script, profile, true);
    assert_eq!(
        from_source, from_bytecode,
        "surface-bytecode diverged from source-eval ({profile:?}) for: {script}"
    );
}

/// Scripts exercising the full injected surface: `$std` reachability, projection identity, the
/// deep-freeze/lock, container ops, and the `json`/`log`/`emit` channels.
const SURFACE_PROBES: &[&str] = &[
    // Every built-in reachable through `$std` (the capability-free always-on subset).
    "function handler() { return json(['money','decimal','text','datetime','list','dict', \
       'crypto','env','secrets','json','log','emit'].every(function (k) { \
       return $std[k] !== undefined; })); }",
    // The curated globals are identity-equal to their `$std` members.
    "function handler() { return json({ m: $ === $std.money, j: json === $std.json, \
       l: log === $std.log, e: emit === $std.emit }); }",
    // `$std` is deep-frozen before the handler runs — a member write is rejected.
    "function handler() { try { $std.money = 1; return json('unfrozen'); } \
       catch (e) { return json('frozen'); } }",
    // The container is locked (non-extensible).
    "function handler() { return json(Object.isFrozen($std)); }",
    // A value-util computation flows through the lazily-materialized wrapper the same either way.
    "function handler() { return json($(10, 'USD').add($(5, 'USD')).to_minor()); }",
    // Containers.
    "function handler() { return json($std.list([3, 1, 2]).length); }",
    // Materialize the remaining value-util wrappers (built from bytecode on the surface path):
    // template + check are not touched by the reachability probe above.
    "function handler() { return json([typeof $std.template, typeof $std.check, \
       typeof $std.decimal, typeof $std.datetime].join(',')); }",
    // A decimal computation through the lazily-materialized wrapper.
    "function handler() { return json($std.decimal('1.10').add('2.20').toString()); }",
    // The diagnostic + effects channels type-check identically.
    "function handler() { log.info('hi {n}', { n: 1 }); emit('k', { v: 2 }); return json('ok'); }",
];

#[test]
fn surface_probes_equivalent_full_profile() {
    for script in SURFACE_PROBES {
        assert_equivalent(script, Profile::Full);
    }
}

#[test]
fn surface_probes_equivalent_deterministic_profile() {
    for script in SURFACE_PROBES {
        assert_equivalent(script, Profile::Deterministic);
    }
}

/// The determinism prune of the ambient authorities is identical on both paths: under
/// `Profile::Deterministic` `$std.datetime.now` and `$std.crypto.uuid` are unreachable.
#[test]
fn determinism_prune_equivalent() {
    let probe = "function handler() { return json({ \
        now: typeof ($std.datetime && $std.datetime.now), \
        uuid: typeof ($std.crypto && $std.crypto.uuid) }); }";
    // Both paths agree, AND the pruned surface is what a deterministic run sees.
    assert_equivalent(probe, Profile::Deterministic);
    let out = run_variant(probe, Profile::Deterministic, true);
    assert!(out.contains("\"now\":\"undefined\""), "{out}");
    assert!(out.contains("\"uuid\":\"undefined\""), "{out}");
}

/// The modified per-request-isolation requirement: with the surface loaded from bytecode, a
/// mutation of a global or a prototype in one request does NOT survive into the next — the fresh
/// context makes bytecode reuse restore compiled code, never retained state.
#[test]
fn precompiled_surface_does_not_leak_state_across_requests() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let surface = compile_surface(&runtime).unwrap_or_else(|_err| unreachable!());
    // Request 1 pollutes a global and a shared prototype.
    let mutate = surface_params(
        &runtime,
        &surface,
        "function handler() { globalThis.__leak = 1; Object.prototype.__poll = 2; \
           return json('mutated'); }",
    );
    drop(run(&mutate).unwrap_or_else(|_err| unreachable!()));
    // Request 2 on the SAME runtime observes a pristine surface.
    // `typeof` (not the raw value) so a clean slot serializes as the string "undefined" rather
    // than being dropped from the JSON object entirely.
    let observe = surface_params(
        &runtime,
        &surface,
        "function handler() { return json({ leak: typeof globalThis.__leak, \
           poll: typeof Object.prototype.__poll }); }",
    );
    let result = run(&observe).unwrap_or_else(|_err| unreachable!());
    let out = match result.outcome {
        ExecOutcome::Success(json) => json,
        ExecOutcome::Error(err) => format!("ERROR: {err:?}"),
    };
    assert!(
        out.contains("\"leak\":\"undefined\""),
        "global leaked: {out}"
    );
    assert!(
        out.contains("\"poll\":\"undefined\""),
        "prototype leaked: {out}"
    );
}
