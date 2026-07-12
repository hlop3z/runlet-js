//! A/B microbenchmark: per-request `Context` construction cost, `Context::full` (all QuickJS
//! intrinsics — the current `engine::run` behavior) vs `Context::base` (the minimal intrinsic
//! floor).
//!
//! Why this is the right measurement for "trim per-request intrinsics". The engine builds a
//! **fresh context every request** (`engine/mod.rs`: `Context::full(params.runtime)`), and the
//! intrinsic set is fixed at construction. Everything that happens *after* construction — surface
//! bytecode injection, lazy `$std`, the handler run — is **byte-identical** regardless of which
//! intrinsics were loaded. So the whole prize of trimming intrinsics is exactly the construction
//! delta measured here: `full_ctor - base_ctor` is the upper bound on the per-request time a
//! curated intrinsic set could ever save.
//!
//! `base` is the extreme floor (it cannot run the real surface — no `JSON`/`RegExp`), so this pair
//! is a construction-only A/B, not an end-to-end one. Read it against the `mux` bench's
//! `baseline_0_calls` arm (the real full-request cost through `LogicHost`): the construction delta
//! as a fraction of that baseline is the maximum RPS headroom lever 1 can yield on this ~CPU-bound
//! path. If it is a small fraction, the precise-minimal-set work (which must keep `Eval` for the
//! intentionally-retained `Function()` constructor, plus `RegExp`/`StringNormalize`/`Json`/`Date`/
//! `TypedArrays` the surface + handlers use, dropping only `Proxy`) is not worth proposing.
//!
//! Run: `cargo bench -p runlet-bench --bench context`

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rquickjs::context::intrinsic;
use rquickjs::{Context, Runtime};

/// Constructs a full-intrinsic context (the current per-request path) and drops it, so the sample
/// captures the full intrinsic build + teardown that every request pays today.
fn construct_full(runtime: &Runtime) {
    let ctx = Context::full(runtime).unwrap_or_else(|_err| unreachable!("full context must build"));
    black_box(&ctx);
    drop(ctx);
}

/// Constructs a base-intrinsic context (the minimal floor) and drops it. The delta to
/// [`construct_full`] is the entire per-request saving trimming intrinsics could yield.
fn construct_base(runtime: &Runtime) {
    let ctx = Context::base(runtime).unwrap_or_else(|_err| unreachable!("base context must build"));
    black_box(&ctx);
    drop(ctx);
}

/// Constructs a context via the builder with `intrinsic::All` — the MOST a curated build can
/// include through the safe rquickjs API. Note `All` is NOT equal to `Context::full`: the builder
/// vocabulary omits `BigInt` + `StringNormalize` (which `JS_NewContext`/`full` add and cannot be
/// re-added without forbidden `unsafe` FFI), and adds `Performance`/`WeakRef`. So `full - all` is
/// (roughly) the cost of the intrinsics a builder-based trim would be UNABLE to keep.
fn construct_all(runtime: &Runtime) {
    let ctx = Context::builder()
        .with::<intrinsic::All>()
        .build(runtime)
        .unwrap_or_else(|_err| unreachable!("all-intrinsic context must build"));
    black_box(&ctx);
    drop(ctx);
}

/// Constructs the realistic SAFE curated set: everything the sandbox is known to lean on
/// (`Eval` for the intentionally-retained `Function()`, `Json`/`RegExp`/`Date`/`MapSet`/
/// `TypedArrays`/`Promise`), dropping only `Proxy` (already stripped post-eval), `Performance`,
/// and `WeakRef`. The delta `all - curated_safe` is the actual prize of the clearly-safe trim; the
/// delta `full - curated_safe` is the prize MINUS the BigInt/StringNormalize regression it incurs.
fn construct_curated_safe(runtime: &Runtime) {
    let ctx = Context::builder()
        .with::<intrinsic::Date>()
        .with::<intrinsic::Eval>()
        .with::<intrinsic::RegExpCompiler>()
        .with::<intrinsic::RegExp>()
        .with::<intrinsic::Json>()
        .with::<intrinsic::MapSet>()
        .with::<intrinsic::TypedArrays>()
        .with::<intrinsic::Promise>()
        .build(runtime)
        .unwrap_or_else(|_err| unreachable!("curated context must build"));
    black_box(&ctx);
    drop(ctx);
}

/// The A/B: full-intrinsic vs base-intrinsic construction, sharing one warm runtime so the only
/// variable is the intrinsic set loaded per context.
fn bench_context(crit: &mut Criterion) {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!("runtime must build"));

    let mut group = crit.benchmark_group("context_construct");
    group.sample_size(100);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("full", |bencher| {
        bencher.iter(|| construct_full(black_box(&runtime)));
    });
    group.bench_function("base", |bencher| {
        bencher.iter(|| construct_base(black_box(&runtime)));
    });
    group.bench_function("all_via_builder", |bencher| {
        bencher.iter(|| construct_all(black_box(&runtime)));
    });
    group.bench_function("curated_safe", |bencher| {
        bencher.iter(|| construct_curated_safe(black_box(&runtime)));
    });
    group.finish();
}

criterion_group!(benches, bench_context);
criterion_main!(benches);
