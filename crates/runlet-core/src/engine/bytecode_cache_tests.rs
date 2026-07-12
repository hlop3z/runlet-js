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
