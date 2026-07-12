//! Throughput/concurrency sweep for the `LogicHost` hot path — the "more workers" question.
//!
//! Not a latency microbench (that's the criterion benches); this measures *aggregate* req/s as
//! concurrency rises, to answer two things the single-op benches can't:
//!
//!   1. COMPUTE regime — does the box's per-request work actually parallelize across cores, or
//!      does it plateau early (a shared-mutex/contention bottleneck)? A pure-compute handler is
//!      driven at rising thread counts; healthy behavior is ~linear scaling up to core count, then
//!      flat. Early plateau = free RPS waiting behind a contention fix.
//!
//!   2. I/O regime — for handlers that block on egress, does adding "workers" (concurrency) raise
//!      throughput? A capability backend that `sleep`s models a network round-trip. Because an
//!      `io.call` holds its runtime/thread for the whole wait (`broker.rs`/`local_io.rs` block_on),
//!      a pool sized to cores leaves CPUs idle while slots are parked; more concurrency should lift
//!      throughput until the residual per-request CPU saturates the cores. This is the real,
//!      workload-gated payoff of raising `engine.pool_size` above core count.
//!
//! The direct-`host.run` path has no bulkhead (that lives in the `runlet` binary), and `JsPool`
//! `acquire()` never blocks (it creates a runtime when the warm set is empty), so concurrency here
//! is exactly the spawned thread count. The pool is warm-sized to cover the widest sweep so a warm
//! runtime is popped rather than cold-created inside the timing window.
//!
//! Run: `cargo run -p runlet-bench --release --bin sweep`

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, available_parallelism};
use std::time::{Duration, Instant};

use rquickjs::{Context as JsContext, Runtime as JsRuntime};

use runlet_core::config::EngineConfig;

/// Hypothesis test: replace musl's contended `malloc` with mimalloc process-wide. If the
/// raw-rquickjs and compute regimes scale with this in place (they were flat/degrading on musl
/// malloc), the multi-core RPS ceiling was allocator lock contention, not QuickJS or runlet-core.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use runlet_core::egress::{Egress, EgressError};
use runlet_core::host::{CapabilitySet, HostSettings, Invocation, LogicHost};
use runlet_core::modules::ModuleRegistry;
use runlet_core::pool::JsPool;
use runlet_core::registry::ScriptRegistry;
use runlet_core::{CapabilityDef, Trust};

/// Widest concurrency the sweep reaches — the warm pool is sized to this so no arm cold-creates
/// runtimes inside its timing window.
const MAX_WORKERS: usize = 128;
/// Simulated egress round-trip time for the I/O regime (a DB/broker call).
const IO_SLEEP: Duration = Duration::from_millis(5);
/// Timed window per data point.
const WINDOW: Duration = Duration::from_millis(1500);

/// A no-op backend (instant return) — the COMPUTE-regime capability, isolates CPU cost from any
/// wait. Mirrors the `mux` bench's `NoopEgress`.
struct NoopEgress;
impl Egress for NoopEgress {
    fn call(&self, _name: &str, _action: &str, _payload: &str) -> Result<String, EgressError> {
        Ok("{}".to_owned())
    }
}

/// A backend that blocks for [`IO_SLEEP`] — models a network egress round-trip so the I/O regime
/// exercises "runtime parked on I/O while a CPU sits idle".
struct SleepEgress;
impl Egress for SleepEgress {
    fn call(&self, _name: &str, _action: &str, _payload: &str) -> Result<String, EgressError> {
        thread::sleep(IO_SLEEP);
        Ok("{}".to_owned())
    }
}

/// Builds one warm host with both capabilities registered (`noop` instant, `slow` sleeping) and a
/// warm pool sized to [`MAX_WORKERS`].
fn build_host() -> LogicHost {
    let mut config = EngineConfig::default();
    config.pool_size = MAX_WORKERS;
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
    let slow = CapabilityDef::new("slow", "", "", Trust::OperatorSupplied)
        .with_backend(Arc::new(SleepEgress));
    LogicHost::builder(pool, Arc::new(ScriptRegistry::default()), settings)
        .capability(noop)
        .capability(slow)
        .build()
        .unwrap_or_else(|_err| unreachable!("capability names are unique"))
}

/// Runs `script` in a tight loop on `workers` threads for `window`, returning aggregate req/s.
fn measure(host: &LogicHost, script: &str, workers: usize, window: Duration) -> f64 {
    let done = AtomicU64::new(0);
    let deadline = Instant::now() + window;
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut local = 0_u64;
                while Instant::now() < deadline {
                    let outcome =
                        host.run(Invocation::inline(script, "{\"n\":1}").caps(CapabilitySet::NONE));
                    black_box(outcome.unwrap_or_else(|_err| unreachable!("invocation must run")));
                    local = local.saturating_add(1);
                }
                let _ = done.fetch_add(local, Ordering::Relaxed);
            });
        }
    });
    let total = done.load(Ordering::Relaxed);
    #[expect(clippy::cast_precision_loss, reason = "throughput report, not exact")]
    let rps = total as f64 / window.as_secs_f64();
    rps
}

/// CONTROL: pure-Rust CPU work, no QuickJS/host at all. Proves whether the 16 logical cores are
/// real and schedulable — if THIS scales ~linearly but the compute regime does not, the
/// bottleneck is a lock in `host.run`, not the container/VM. Returns aggregate Mops/s.
fn measure_control(workers: usize, window: Duration) -> f64 {
    let done = AtomicU64::new(0);
    let deadline = Instant::now() + window;
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut acc = 0_u64;
                let mut ops = 0_u64;
                while ops % 4096 != 0 || Instant::now() < deadline {
                    // A dependent-op loop the optimizer can't hoist or vectorize away.
                    acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    ops = ops.saturating_add(1);
                }
                black_box(acc);
                let _ = done.fetch_add(ops, Ordering::Relaxed);
            });
        }
    });
    #[expect(clippy::cast_precision_loss, reason = "report only")]
    let mops = done.load(Ordering::Relaxed) as f64 / window.as_secs_f64() / 1_000_000.0;
    mops
}

/// CONTROL 2: raw rquickjs, no runlet-core. Each thread owns a private `Runtime` + `Context`
/// (distinct QuickJS heaps, distinct per-runtime locks) and evals a trivial expression in a loop.
/// If this scales but the COMPUTE regime does not, the serialization is in runlet-core (the shared
/// `moka` bytecode cache is the prime suspect); if this is ALSO flat, it's a QuickJS-global lock.
fn measure_rawjs(workers: usize, window: Duration) -> f64 {
    let done = AtomicU64::new(0);
    let deadline = Instant::now() + window;
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let rt = JsRuntime::new().unwrap_or_else(|_err| unreachable!("runtime"));
                let ctx = JsContext::full(&rt).unwrap_or_else(|_err| unreachable!("context"));
                let mut local = 0_u64;
                while Instant::now() < deadline {
                    ctx.with(|c| {
                        let out: i32 = c.eval("1+1").unwrap_or_else(|_err| unreachable!("eval"));
                        black_box(out);
                    });
                    local = local.saturating_add(1);
                }
                let _ = done.fetch_add(local, Ordering::Relaxed);
            });
        }
    });
    #[expect(clippy::cast_precision_loss, reason = "report only")]
    let rps = done.load(Ordering::Relaxed) as f64 / window.as_secs_f64();
    rps
}

/// A pure-compute handler: no `io.call`, touches no value-util (lazy `$std` stays unbuilt).
const COMPUTE: &str = "function handler(ctx) { return json(ctx.n + 1); }";
/// An I/O-bound handler: one `io.call` to the sleeping backend, dominated by the 5 ms wait.
const IO: &str = "function handler(ctx) { io.call('slow', 'ping', { x: ctx.n }); return json(ctx.n + 1); }";

fn main() {
    let cores = available_parallelism().map_or(1, std::num::NonZero::get);
    let host = build_host();

    // Warm the pool + code paths so the first timed point isn't paying cold costs.
    let _ = measure(&host, COMPUTE, cores, Duration::from_millis(400));

    println!("cores = {cores}, warm pool = {MAX_WORKERS}, window = {WINDOW:?}\n");

    println!("== CONTROL: pure-Rust CPU (no QuickJS) — are the 16 cores real & schedulable? ==");
    println!("{:>8} | {:>12} | {:>10}", "workers", "Mops/s", "vs 1x");
    let ctrl_base = measure_control(1, WINDOW);
    for &w in &[1_usize, 2, 4, 8, 16] {
        let mops = if w == 1 { ctrl_base } else { measure_control(w, WINDOW) };
        println!("{w:>8} | {mops:>12.1} | {:>9.2}x", mops / ctrl_base);
    }
    println!();

    println!("== CONTROL 2: raw rquickjs (private Runtime+Context/thread) — does QuickJS scale? ==");
    println!("{:>8} | {:>12} | {:>10}", "workers", "eval/s", "vs 1x");
    let raw_base = measure_rawjs(1, WINDOW);
    for &w in &[1_usize, 2, 4, 8, 16] {
        let rps = if w == 1 { raw_base } else { measure_rawjs(w, WINDOW) };
        println!("{w:>8} | {rps:>12.0} | {:>9.2}x", rps / raw_base);
    }
    println!();

    println!("== COMPUTE regime (no io) — does per-request work scale across cores? ==");
    println!("{:>8} | {:>12} | {:>10} | {:>10}", "workers", "req/s", "vs 1x", "eff/core");
    let base = measure(&host, COMPUTE, 1, WINDOW);
    for &w in &[1_usize, 2, 4, 8, 16, 24, 32] {
        let rps = if w == 1 { base } else { measure(&host, COMPUTE, w, WINDOW) };
        let speedup = rps / base;
        let ideal = w.min(cores);
        #[expect(clippy::cast_precision_loss, reason = "report only")]
        let eff = speedup / ideal as f64;
        println!("{w:>8} | {rps:>12.0} | {speedup:>9.2}x | {:>9.0}%", eff * 100.0);
    }

    println!("\n== I/O regime (one 5ms egress call) — does adding workers raise throughput? ==");
    println!(
        "{:>8} | {:>12} | {:>14}",
        "workers", "req/s", "vs cores(16)"
    );
    let io_base = measure(&host, IO, cores, WINDOW);
    for &w in &[cores, cores * 2, cores * 4, cores * 8] {
        let rps = if w == cores { io_base } else { measure(&host, IO, w, WINDOW) };
        println!("{w:>8} | {rps:>12.0} | {:>13.2}x", rps / io_base);
    }
}
