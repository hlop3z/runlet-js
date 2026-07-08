//! Microbenchmark: the capability-mux hot path (`composable-capability-core`).
//!
//! The registry mux adds, per `io.call`, one `HashMap<name, Route>` lookup + a short
//! `Option::or` fallback chain on top of the pre-existing dynamic `Egress` dispatch
//! (`CapabilityRegistry::dispatch`). This bench drives the **real** `LogicHost` through the public
//! API and measures the per-`io.call` cost against a no-op backend, so the mux overhead can be
//! read as the delta between handlers that make 0, 1, and 10 `io.call`s.
//!
//! Every arm shares one warm host, so pooled-runtime acquisition, fresh-context creation, and mux
//! injection are a constant offset present in all arms; the meaningful figure is the **difference**
//! between arms — the marginal cost of an `io.call` routed through the mux to a backend that does
//! nothing. If the mux lookup were a hot-path concern, that per-call delta would be large; it is
//! dominated instead by the string-in/string-out FFI (`JSON.stringify` → `__io` → `JSON.parse`).
//!
//! Run: `cargo bench -p runlet-bench --bench mux`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};

use runlet_core::config::EngineConfig;
use runlet_core::egress::{Egress, EgressError};
use runlet_core::host::{CapabilitySet, HostSettings, Invocation, LogicHost};
use runlet_core::modules::ModuleRegistry;
use runlet_core::pool::JsPool;
use runlet_core::registry::ScriptRegistry;
use runlet_core::{CapabilityDef, Trust};

/// A backend that does nothing but return an empty JSON object — isolates the mux + FFI cost from
/// any real I/O, so the per-call delta is the routing + boundary crossing, not a network round-trip.
struct NoopEgress;

impl Egress for NoopEgress {
    fn call(&self, _name: &str, _action: &str, _payload_json: &str) -> Result<String, EgressError> {
        Ok("{}".to_owned())
    }
}

/// Builds a warm `LogicHost` registering one capability named `noop` bound to [`NoopEgress`], so a
/// script's `io.call('noop', …)` routes through the real mux to the no-op backend.
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
    let noop = CapabilityDef::new("noop", "", "", Trust::OperatorSupplied)
        .with_backend(Arc::new(NoopEgress));
    LogicHost::builder(pool, Arc::new(ScriptRegistry::default()), settings)
        .capability(noop)
        .build()
        .unwrap_or_else(|_err| unreachable!("preset has unique names"))
}

/// Runs one invocation of `script` on the warm host and black-boxes the outcome.
fn run_once(host: &LogicHost, script: &str) {
    let outcome = host.run(Invocation::inline(script, "{\"n\":1}").caps(CapabilitySet::NONE));
    black_box(outcome.unwrap_or_else(|_err| unreachable!("invocation must run")));
}

/// Baseline handler (no `io.call`) vs handlers making 1 and 10 mux-routed `io.call`s. The
/// per-call cost is the delta between the arms.
fn bench_mux(crit: &mut Criterion) {
    let host = build_host();

    // A pure-compute baseline: the same host.run offset (context creation + mux injection) without
    // crossing the io.call boundary even once.
    const BASELINE: &str = "function handler(ctx) { return json(ctx.n + 1); }";
    // One io.call routed through the mux to the no-op backend.
    const ONE_CALL: &str =
        "function handler(ctx) { io.call('noop', 'ping', { x: ctx.n }); return json(ctx.n + 1); }";
    // Ten io.calls — the per-call cost read off the slope vs ONE_CALL / BASELINE.
    const TEN_CALLS: &str = "function handler(ctx) { \
        for (var i = 0; i < 10; i++) { io.call('noop', 'ping', { x: i }); } \
        return json(ctx.n + 1); }";

    let mut group = crit.benchmark_group("mux");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("baseline_0_calls", |bencher| {
        bencher.iter(|| run_once(&host, black_box(BASELINE)));
    });
    group.bench_function("one_io_call", |bencher| {
        bencher.iter(|| run_once(&host, black_box(ONE_CALL)));
    });
    group.bench_function("ten_io_calls", |bencher| {
        bencher.iter(|| run_once(&host, black_box(TEN_CALLS)));
    });
    group.finish();
}

criterion_group!(benches, bench_mux);
criterion_main!(benches);
