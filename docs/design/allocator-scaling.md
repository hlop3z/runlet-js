# Allocator scaling: why the box swaps the global allocator to mimalloc

Companion to [resilience.md](resilience.md) — the Tier 1 bulkhead admits concurrent work; this
note is why that admitted concurrency actually reaches the cores instead of serializing.

> **Behavioral contract → [`openspec/specs/resilience`](../../openspec/specs/resilience/spec.md),
> "Parallel execution scaling".** That requirement is the testable WHAT (throughput scales with
> workers; executions don't serialize on a process-global resource; single-thread not regressed).
> This note is the **rationale**: the measured root cause, the sweep methodology, and the
> before/after figures a spec doesn't carry.

## The symptom

On a 16-core / 16 GB Alpine container, compute-only throughput through `LogicHost::run` was **flat
at ~1,400 req/s from 1 to 32 worker threads**. Adding cores bought nothing: per-core efficiency at
16 workers collapsed to ~6%. The Tier 1 bulkhead and Tier 5 per-partition fairness were admitting
real parallel work that then executed one-at-a-time — 16 cores doing the work of ~1. Because the
release image is **musl-static** (Alpine builder → distroless), this ceiling shipped in production.

## Isolating the cause

`crates/runlet-bench/src/bin/sweep.rs` drives a rising thread count against a pure-compute handler
(`function handler(ctx){ return json(ctx.n + 1); }`, no `io.call`, no value-util so lazy `$std`
stays unbuilt) and reports aggregate req/s. Two **controls** in the same binary localize where the
serialization lives — each is deliberately below a different layer:

- **CONTROL: pure-Rust CPU** (a dependent-multiply loop, no QuickJS, no host) — proves the logical
  cores are real and schedulable. It scaled **13.8× at 16 threads**, so the ceiling is not a
  container/cgroup cap (`cpu.max` was unlimited).
- **CONTROL 2: raw rquickjs** — each thread owns a fully **private `Runtime` + `Context`** (distinct
  QuickJS heaps, distinct per-runtime locks) and evals `1+1` in a loop. If this scaled but COMPUTE
  did not, the bottleneck would be a `runlet-core` lock (the shared `moka` bytecode cache was the
  prime suspect); if it was *also* flat, a QuickJS-global eval lock. On musl malloc it **degraded to
  0.22×** — so the serialization is neither a runlet-core lock nor a QuickJS eval lock. It sits
  **below both**, in a resource shared even by independent heaps: the process-global allocator.

QuickJS allocates and frees a large number of small objects per request, and musl's `malloc`
serializes those calls hard under concurrency (a single global arena / lock, by design — musl
optimizes for size and simplicity, not many-thread alloc churn). That is the shared resource the
private-heap control still contended on.

## The fix

Register a scalable, per-thread-arena allocator as the process `#[global_allocator]`. Swapping in
**mimalloc** flipped every regime:

| workers | musl malloc | mimalloc            |
| ------- | ----------- | ------------------- |
| 1       | 1,363 req/s | 2,366 (**1.7×**)    |
| 16      | 1,370 req/s | **23,041 (~17×)**   |

Per-core efficiency at 16 workers: **6% → 61%**. The raw-rquickjs control moved **0.22× → 8.32×** on
the same swap, confirming the allocator was the shared bottleneck (independent heaps now scale). And
mimalloc is **~1.7× faster single-threaded** — the uncontended path is not regressed, it improves
(smaller-object fast paths, per-thread free-lists).

The whole change is one line in the `runlet` **binary** (only a binary may set a global allocator; a
library setting one would poison every downstream consumer of `runlet-core`):

```rust
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

The seam that makes this cover *QuickJS's* allocations too is rquickjs's **`rust-alloc`** feature
(root `Cargo.toml`): with it, every QuickJS allocation flows through the Rust `GlobalAlloc` boundary
rather than calling C `malloc` directly. So one allocator serves both Rust and QuickJS, and — the
key point for the shipping artifact — **no musl C-`malloc` symbol interposition is needed** (the
notoriously hard part of a custom allocator on static-musl is sidestepped entirely). The default
(non-`secure`) mimalloc build is used: the sandbox's security boundary is QuickJS + OS limits, not
the heap allocator, and `secure` mode costs throughput. No `unsafe` in our code — the `GlobalAlloc`
impl and its unsafety live inside the mimalloc crate, so the no-`unsafe` lint gauntlet holds.

## End-to-end confirmation: the real HTTP `/execute` path

The 17× above isolates the *allocator* variable through `host.run` — no HTTP, no axum/tokio, no
serde. To confirm the fix survives the full stack, the same allocator A/B was re-measured through
**`POST /execute` on the shipping static-musl distroless image** (16-core host, `oha` load generator
sharing the container's network namespace, 4 s windows, keep-alive), for both a trivial handler and
an allocation-heavy one. Aggregate req/s at rising connection concurrency:

| concurrency | musl malloc | mimalloc | ratio |
| ----------- | ----------- | -------- | ----- |
| 1           | 6,723       | 8,440    | 1.26× |
| 4           | 22,790      | 28,586   | 1.25× |
| 8           | 40,309      | 51,700   | 1.28× |
| 16          | 53,713      | 94,407   | 1.76× |
| 24          | 65,545      | 114,917  | 1.75× |
| 32          | 65,011      | 126,825  | 1.95× |
| 48          | 68,248      | 153,264  | 2.25× |

Three things hold, matching the `resilience` spec's scenarios:

1. **Scales with workers.** mimalloc rises monotonically past core count; musl **plateaus at
   ~65–68k** from c≈16 onward — that plateau *is* the allocator becoming the ceiling once enough
   concurrent requests contend on it.
2. **No serialization collapse.** The gap **widens with concurrency** (1.26× → 2.25×): at low load
   there is little contention (mimalloc wins only by its faster fast-path); as load rises the musl
   lock convoy caps throughput while mimalloc keeps scaling on per-thread arenas.
3. **Single-thread not regressed.** 8,440 vs 6,723 req/s at c=1 — the uncontended path *improves*.

**Why the HTTP multiplier (~2.25×) is smaller than the bench's 17×.** A trivial handler
(`ctx.n*2`) and a deliberately allocation-heavy one (build 200 objects, map/filter/reduce, stringify)
produced **near-identical** end-to-end req/s (c=1: 8,351 vs 8,440 for mimalloc; 6,680 vs 6,723 for
musl). So per-request throughput here is gated by **fixed per-request machinery** — HTTP parse, a
fresh QuickJS `Context` per request, serde of script+context, response serialization — not the
handler body. The allocator's end-to-end win comes from *that* fixed machinery's allocation churn
scaling under concurrency, which is a smaller share of the whole than the tight allocation loop the
`host.run` bench isolated. Both numbers are real; they measure different layers. The bench proves the
allocator was the multi-core ceiling; the HTTP A/B proves the fix survives the full stack (removes
the plateau, improves every point, never regresses).

## Method notes / caveats

- Numbers are from the **dynamic Alpine dev container via direct `host.run`** — no HTTP, no
  axum/tokio, no bulkhead (the sweep path has none; `JsPool::acquire()` never blocks, so concurrency
  is exactly the spawned thread count). They isolate the *allocator* variable, not end-to-end RPS.
  The pool is warm-sized to the widest sweep so no arm cold-creates runtimes inside its window.
- The uplift is **larger on musl than on glibc** (glibc's `malloc` already has per-thread arenas), so
  a glibc build sees a smaller — still positive — gain. Not relevant to the shipping artifact, which
  is musl.
- **RSS**: mimalloc reserves per-thread arenas, so baseline RSS rises modestly. Accepted for ~17×
  throughput; tunable via `MIMALLOC_*` env if a deployment is memory-constrained.
- **Residual ~39% per-core inefficiency at 16 cores** (mimalloc reaches ~61%, not 100%) is out of
  scope here — a separate, later investigation. Candidates: shared `moka` bytecode-cache contention,
  per-request `Arc`/`Mutex` buffers.

## Supply chain

`mimalloc` + `libmimalloc-sys` are **MIT** (Microsoft Research), covered by `cargo vet` exemptions
(pinned to the locked versions) and clean under `cargo deny`. The `cc` toolchain the C source needs
is already in the build image (`aws-lc-sys` requires it too).
