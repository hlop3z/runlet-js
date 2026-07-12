## Context

The box was measured (`crates/runlet-bench/src/bin/sweep.rs`, 16-core / 16 GB Alpine container,
direct `host.run`) to not scale execution across cores: compute throughput is flat at ~1,400 req/s
from 1 to 32 worker threads (per-core efficiency ~6% at 16 workers). Three controls in the same sweep
localized the cause:

- **Pure-Rust CPU** scaled 13.8× at 16 threads → the cores are real and schedulable; not a container
  cap (cgroup `cpu.max` is unlimited).
- **Raw rquickjs** with a fully private `Runtime`+`Context` per thread (independent heaps, independent
  per-runtime locks) *degraded* 0.22× under threads → the serialization is neither a runlet-core lock
  nor a QuickJS eval lock; it is below both.
- **Swapping the global allocator to mimalloc** flipped raw rquickjs to 8.32× and the full compute
  regime to ~17× at 16 cores (23,041 vs 1,370 req/s), and ~1.7× single-threaded (2,366 vs 1,363).

Root cause: QuickJS allocates and frees a large number of small objects per request, and the musl
allocator in the Alpine/distroless build serializes those calls under concurrency. The Tier 1
concurrency bulkhead admits parallel work that then serializes on the allocator. Because the release
image is **musl-static**, this ceiling ships in production today.

Constraints: strict lint gauntlet (no `unsafe` in our code, no `#[allow]`); Docker-only builds;
libraries must not set a global allocator (only the final binary may). Rationale of measured
trade-offs belongs in `docs/design/`, linked from the spec.

## Goals / Non-Goals

**Goals:**
- Restore parallel execution scaling across cores by removing global-allocator lock contention.
- Keep the change confined to the `runlet` binary; leave `runlet-core`/`runlet-wire`, the QuickJS
  engine, capabilities, config, and the isolation model untouched.
- Prove the fix on the real shipping artifact (static-musl distroless) and through the real HTTP
  `/execute` path, not only in the dev container / `host.run` bench.
- Keep supply-chain gates (`cargo vet`, `cargo deny`) green.

**Non-Goals:**
- No change to `/execute` semantics, sandbox isolation, determinism, or the fresh-Context-per-request
  model.
- No allocator tuning of QuickJS internals or rquickjs; no per-runtime custom allocator.
- Not chasing the residual ~39% per-core inefficiency at 16 cores (mimalloc gets to ~61%); that is a
  separate, later investigation.

## Decisions

### Decision: Scalable global allocator — Adopt `mimalloc`

- **Status**: approved
- **Why**: the only candidate measured on our workload (~17× multi-core, ~1.7× single-thread via
  `crates/runlet-bench/src/bin/sweep.rs`); actively maintained (v0.1.52, 2026-05, MIT / Microsoft
  Research, per-thread free-lists / sharded arenas); and the cleanest musl-static path because
  rquickjs runs with `rust-alloc`, so every allocation — Rust *and* QuickJS — flows through the Rust
  `#[global_allocator]` and no musl C-`malloc` symbol interposition is needed.
- **Considered**: `tikv-jemallocator` (mature, battle-tested, but heavier and historically awkward on
  musl-static, and unmeasured here — kept as the documented fallback if mimalloc cannot link static);
  `snmalloc-rs` (strong design, smaller ecosystem, least proven on musl-static distroless, unmeasured);
  the system musl allocator (rejected — it is the measured cause of the ceiling); a hand-rolled
  allocator (rejected outright — never build an allocator).
- **Isolation**: the whole choice lives behind one line —
  `#[global_allocator] static GLOBAL: mimalloc::MiMalloc` in `crates/runlet/src/main.rs`. No other
  crate references the allocator, so swapping to the jemalloc fallback is a one-line change. The
  rquickjs `rust-alloc` feature (`Cargo.toml`) is the seam that carries QuickJS's C allocations through
  that Rust boundary.

A memory allocator is performance- and reliability-critical infrastructure — squarely "Adopt a mature
tool" on the build-vs-adopt gate. mimalloc directly addresses the many-threads/heavy-alloc-churn
profile QuickJS exhibits.

### Decision: Register the allocator in the `runlet` binary, unconditionally

`#[global_allocator] static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;` lives in
`crates/runlet/src/main.rs`. Rationale: only a binary may set a global allocator (a library setting
one would poison every downstream consumer of `runlet-core`); and there is exactly one deployment
profile, so a cargo feature flag would add surface for no operational benefit. The default
(non-`secure`) mimalloc build is used — the sandbox's security boundary is QuickJS + OS limits, not
the heap allocator, and `secure` mode costs throughput.

The `#[global_allocator]` attribute on a plain `static` requires no `unsafe` block in our code (the
`GlobalAlloc` impl and its unsafety are inside the mimalloc crate), so the no-`unsafe` lint holds.

### Decision: Verify on the real artifact and path before declaring done

The uplift is proven only in the dynamic Alpine dev container via `host.run`. Before archive:
1. Confirm mimalloc's bundled C links and initializes in the **static-musl → distroless** release
   `Dockerfile` (may need a build flag on the static target; if it cannot link, fall back to jemalloc
   or ship dynamic-musl).
2. Re-measure end-to-end through HTTP `/execute` under load (Python harness or `oha`/`wrk`), not just
   `host.run`, to confirm the number survives the axum/tokio/serialization layers.
3. Record the measured before/after in a `docs/design/` note (spec convention: the WHY lives there),
   and update the stale RPS figures.

## Risks / Trade-offs

- **mimalloc may not link on static-musl distroless** → Reduced by rquickjs `rust-alloc`: because all
  allocations (Rust and QuickJS) flow through the Rust `#[global_allocator]`, we do NOT need to
  interpose the musl C `malloc` symbol (the notoriously hard part on musl-static). The residual risk
  is only "does the `mimalloc`/`libmimalloc-sys` C source compile+link on the static target", which is
  well-supported. Mitigation: still verify in the release Dockerfile early (task 4.1); fallbacks are
  jemalloc, snmalloc, or dynamic-musl (task 4.2).
- **Higher baseline RSS** (mimalloc reserves per-thread arenas) → Mitigation: measure container RSS in
  the release image; mimalloc exposes env tunables (`MIMALLOC_*`) and the footprint is modest relative
  to N warm QuickJS runtimes. Accept a small RSS increase for ~17× throughput.
- **New C dependency (`libmimalloc-sys`) in a supply-chain-audited tree** → Mitigation: add a
  `cargo vet` audit/exemption and confirm `cargo deny`; the `cc` toolchain is already present in the
  build image (aws-lc-sys needs it too).
- **Uplift smaller on glibc hosts** than on musl → Not applicable to the shipping artifact (musl), but
  worth noting for anyone running a glibc build; mimalloc still helps there, just less dramatically.
- **Residual inefficiency at high core counts** (~61% per-core at 16) → Out of scope here; flagged for
  a follow-up (candidates: shared moka bytecode-cache contention, per-request Arc/Mutex buffers).
