//! Microbenchmark: `/batch` fan-out cost vs batch size (`batch-execute-endpoint`, design open
//! question — pick the default `max_batch_items`).
//!
//! The `/batch` endpoint is a fan-out over the same `LogicHost::run` port each single `/execute`
//! uses: a batch of N items is N independent invocations, bounded in concurrency by the existing
//! bulkhead / per-partition fair share. The HTTP + async orchestration (axum, `tokio::JoinSet`,
//! per-item admission) lives in the `runlet` binary and is not reachable from this deterministic-core
//! bench; what this measures is the **per-item execution cost that the fan-out multiplies**, at
//! several batch sizes, so an operator can reason about the worst-case a `max_batch_items` choice
//! admits: `max_batch_items × per-item-time` bounds how long a batch can hold its share of the pool
//! (the concrete reason the default is kept modest).
//!
//! Each arm runs a full batch of N warm invocations and reports **per-item** time via criterion's
//! throughput (elements = N). A flat per-item line across N confirms the design's "a batch is exactly
//! N single requests in cost" invariant — the fan-out adds no per-item overhead, so the only knob is
//! the latency budget the operator is willing to admit.
//!
//! Run: `cargo bench -p runlet-bench --bench batch`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use runlet_core::config::EngineConfig;
use runlet_core::host::{CapabilitySet, HostSettings, Invocation, LogicHost};
use runlet_core::modules::ModuleRegistry;
use runlet_core::pool::JsPool;
use runlet_core::registry::ScriptRegistry;

/// Builds a warm `LogicHost` with default limits and no capabilities — a batch item here is a pure
/// deterministic transform, isolating the per-item execution cost the fan-out multiplies.
fn build_host() -> LogicHost {
    let mut config = EngineConfig::default();
    config
        .resolve_limits()
        .unwrap_or_else(|_err| unreachable!("default engine limits must resolve"));
    let modules = Arc::new(ModuleRegistry::default());
    let pool = JsPool::new(config, modules).unwrap_or_else(|_err| unreachable!("pool must build"));
    let settings = HostSettings {
        limits: config,
        allow_private_targets: false,
    };
    LogicHost::new(pool, Arc::new(ScriptRegistry::default()), settings)
}

/// Runs one invocation of `script` on the warm host and black-boxes the outcome.
fn run_once(host: &LogicHost, script: &str) {
    let outcome = host.run(Invocation::inline(script, "{\"n\":1}").caps(CapabilitySet::NONE));
    black_box(outcome.unwrap_or_else(|_err| unreachable!("invocation must run")));
}

/// Runs a whole batch of `size` items (each the same transform) through the warm host — the fan-out
/// the `/batch` endpoint performs over `LogicHost::run`, minus the async orchestration.
fn run_batch(host: &LogicHost, size: usize, script: &str) {
    for _item in 0..size {
        run_once(host, black_box(script));
    }
}

/// Per-item fan-out cost at batch sizes spanning the plausible `max_batch_items` range. A flat
/// per-item line confirms the fan-out adds no per-item overhead (batch = N single executes).
fn bench_batch(crit: &mut Criterion) {
    let host = build_host();
    // A representative per-row transform (the batch endpoint's canonical use case: ETL / bulk map).
    const TRANSFORM: &str = "function handler(ctx) { return json(ctx.n * 2 + 1); }";

    let mut group = crit.benchmark_group("batch");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for size in [1_usize, 10, 25, 100] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &size,
            |bencher, &size| {
                bencher.iter(|| run_batch(&host, size, TRANSFORM));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_batch);
criterion_main!(benches);
