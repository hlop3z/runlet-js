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
        surface: None,
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
    let exec =
        run_exec("function handler() { emit(\"finding\", { id: 7 }); throw new Error(\"boom\"); }");
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
