use super::{ExecOutcome, ExecParams, Profile, run};
use rquickjs::Runtime;
use std::time::Duration;

/// Runs `script` under `profile` and returns the success `data`/`error` envelope JSON.
fn run_profiled(script: &str, profile: Profile) -> String {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let params = ExecParams {
        runtime: &runtime,
        bytecode_cache: None,
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

/// Convenience: run under the full profile.
fn run_script(script: &str) -> String {
    run_profiled(script, Profile::Full)
}

/// Every built-in is reachable through `$std` (the always-on subset in the capability-free build).
#[test]
fn every_builtin_reachable_through_std() {
    let out = run_script(
        "function handler() { return json({ \
               present: ['money','decimal','text','datetime','list','dict', \
                         'crypto','env','secrets','json','log','emit'] \
                 .every(function (k) { return $std[k] !== undefined; }) }); }",
    );
    assert!(out.contains("\"present\":true"), "{out}");
}

/// The curated globals are the SAME object references as their `$std` members.
#[test]
fn exposed_globals_are_identity_equal() {
    let out = run_script(
        "function handler() { return json({ \
               money: $ === $std.money, json: json === $std.json, \
               log: log === $std.log, emit: emit === $std.emit }); }",
    );
    assert!(out.contains("\"money\":true"), "{out}");
    assert!(out.contains("\"json\":true"), "{out}");
    assert!(out.contains("\"log\":true"), "{out}");
    assert!(out.contains("\"emit\":true"), "{out}");
}

/// The former bare util globals are removed — reachable only via `$std` (or `$` for money).
#[test]
fn former_bare_globals_are_absent() {
    let out = run_script(
        "function handler() { return json( \
               ['money','Decimal','datetime','text','list','dict','io','http','s3'] \
                 .map(function (k) { return typeof globalThis[k]; })); }",
    );
    // Nine `"undefined"` entries — no former bare util global survives.
    assert_eq!(
        out.matches("undefined").count(),
        9,
        "no former bare util global is defined: {out}"
    );
}

/// Destructuring the namespace yields the same util objects as member access.
#[test]
fn destructuring_the_namespace_works() {
    let out = run_script(
        "function handler() { const { list, dict } = $std; \
               return json({ list: list === $std.list, dict: dict === $std.dict, \
                 works: list([3,1,2]).sort_by().to_array()[0] }); }",
    );
    assert!(
        out.contains("\"list\":true") && out.contains("\"dict\":true"),
        "{out}"
    );
    assert!(out.contains("\"works\":1"), "{out}");
}

/// Under `Profile::Deterministic` every prunable authority is unreachable via every path, and a
/// no-arg `Date()` throws — while the pure surface (parse/hash/list) still works (task 3.3).
#[test]
fn deterministic_profile_prunes_all_authorities() {
    let out = run_profiled(
        "function handler() { \
               var dateThrows = false; \
               try { new Date(); } catch (e) { dateThrows = true; } \
               return json({ \
                 now: typeof $std.datetime.now === 'undefined', \
                 uuid: typeof $std.crypto.uuid === 'undefined', \
                 random: typeof Math.random === 'undefined', \
                 dateThrows: dateThrows, \
                 stillHashes: $std.crypto.sha256('x').length === 64 }); }",
        Profile::Deterministic,
    );
    assert!(out.contains("\"now\":true"), "datetime.now pruned: {out}");
    assert!(out.contains("\"uuid\":true"), "crypto.uuid pruned: {out}");
    assert!(out.contains("\"random\":true"), "Math.random pruned: {out}");
    assert!(
        out.contains("\"dateThrows\":true"),
        "no-arg Date() throws: {out}"
    );
    assert!(
        out.contains("\"stillHashes\":true"),
        "pure crypto survives: {out}"
    );
}

/// The frozen `$std` cannot be mutated/extended, and a locked global cannot be reassigned.
#[test]
fn std_is_frozen_and_globals_locked() {
    let out = run_script(
        "function handler() { \
               var origLog = $std.log; \
               try { $std.newThing = 1; } catch (e) {} \
               try { $std.money = null; } catch (e) {} \
               try { log = 5; } catch (e) {} \
               return json({ \
                 noAdd: $std.newThing === undefined, \
                 moneyKept: typeof $std.money === 'function', \
                 logKept: log === origLog }); }",
    );
    assert!(
        out.contains("\"noAdd\":true"),
        "frozen: no new member: {out}"
    );
    assert!(
        out.contains("\"moneyKept\":true"),
        "frozen: member unchanged: {out}"
    );
    assert!(out.contains("\"logKept\":true"), "global lock holds: {out}");
}
