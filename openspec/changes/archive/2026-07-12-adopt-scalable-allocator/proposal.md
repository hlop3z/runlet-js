## Why

The box does not scale execution throughput across CPU cores: measured on a 16-core host, compute
throughput is flat at ~1,400 req/s from 1 to 32 worker threads (per-core efficiency collapses to
~6%). The cause is global allocator lock contention — QuickJS allocates/frees heavily per request,
and the musl allocator in the Alpine/distroless build serializes those calls under concurrency. The
concurrency tiers (bulkhead, per-partition fairness) admit parallel work that then serializes on the
allocator, so 16 cores do the work of ~1. This ships in production today (the release image is
musl-static). Swapping the global allocator to a scalable, per-thread-arena allocator (mimalloc)
restores parallel scaling — measured ~17× on 16 cores and ~1.7× single-threaded — with no rquickjs
change, no `unsafe`, and no isolation trade-off.

## What Changes

- Register a scalable global allocator (`mimalloc`) as `#[global_allocator]` in the `runlet` binary,
  replacing the default (musl) system allocator process-wide.
- Add the `mimalloc` dependency to the `runlet` crate and cover it under supply-chain review
  (`cargo vet` / `cargo deny`).
- Verify the allocator links and initializes in the distroless **static-musl** release image (the
  dev container is dynamic Alpine; the static target is the shipping artifact and must be confirmed).
- Establish a behavioral guarantee that admitted concurrent executions run in parallel across
  available cores (throughput scales with cores), rather than serializing on a shared allocator.
- No change to `/execute` request/response semantics, sandbox isolation, capabilities, or configuration
  surface. The fresh-Context-per-request isolation model is untouched.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `resilience`: add a parallel-execution-scaling requirement — the system SHALL execute admitted
  concurrent requests in parallel across available CPU cores (aggregate throughput scales with the
  worker count up to core count), so the Tier 1 bulkhead's admitted concurrency is not negated by a
  serialized execution substrate.

## Impact

- **Code**: `crates/runlet/src/main.rs` (add `#[global_allocator]`); `crates/runlet/Cargo.toml`
  (new `mimalloc` dependency). `runlet-core`, `runlet-wire`, the QuickJS engine, and all capability
  code are untouched — the allocator is process-global and transparent to them.
- **Dependencies**: adds `mimalloc` + `libmimalloc-sys` (C, built via `cc`). Supply-chain gate
  (`cargo vet` exemption/audit, `cargo deny`) must be updated so `task supply-chain` stays green.
- **Build/release**: the release `Dockerfile` (musl-static → distroless) must be confirmed to build
  and run with the bundled mimalloc C source; may require a build flag on the static target.
- **Performance**: ~17× multi-core / ~1.7× single-thread compute throughput uplift (measured via
  `crates/runlet-bench/src/bin/sweep.rs`); supersedes the stale ~2.4k RPS baseline. No latency
  regression (single-thread improves).
- **Behavioral contract**: no change to outputs, isolation, or determinism — a purely non-functional
  (throughput) change.
