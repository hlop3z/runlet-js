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
        surface: None,
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
