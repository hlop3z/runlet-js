## 1. Add the allocator to the runlet binary

- [x] 1.1 Add `mimalloc = "0.1"` (default features, non-`secure`) to `crates/runlet/Cargo.toml` dependencies
- [x] 1.2 Register `#[global_allocator] static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;` at the top of `crates/runlet/src/main.rs` (no `unsafe` block needed)
- [x] 1.3 Confirm the change is binary-only — `runlet-core`/`runlet-wire` set no global allocator and are untouched

## 2. Build & lint gates (Docker)

- [x] 2.1 `cargo build -p runlet` in the Alpine dev container builds clean with mimalloc linked
- [x] 2.2 `cargo clippy` (project gate) passes — verify the `static GLOBAL` line trips no `unsafe`/restriction lint (`clippy -p runlet` clean, exit 0; the two warnings are in the untracked `runlet-bench` WIP crate, not this change)
- [x] 2.3 `cargo test` passes unchanged (250 tests, 0 failures — allocator swap is behaviorally transparent)
- [x] 2.4 `cargo fmt --all --check` clean for the change-scoped crates (`runlet`/`runlet-core`/`runlet-wire` exit 0; remaining diffs are in the untracked `runlet-bench` WIP, out of scope)

## 3. Supply chain

- [x] 3.1 Run `task supply-chain`; add a `cargo vet` audit/exemption for `mimalloc` + `libmimalloc-sys` — exemptions already present + version-matched (`mimalloc` 0.1.52, `libmimalloc-sys` 0.1.49); `cargo vet` → `Vetting Succeeded`. `cargo vet prune` would strip 285 unrelated pre-existing exemptions (whole-tree cleanup, out of scope for this change) — reverted, left for a dedicated change.
- [x] 3.2 Confirm `cargo deny` passes (license MIT, no advisory) and re-run `cargo vet prune` as needed — `cargo deny check licenses bans sources` → `bans ok, licenses ok, sources ok`; `cargo deny check advisories` → `advisories ok` (mimalloc/libmimalloc-sys both MIT, no RUSTSEC advisory)

## 4. Release-image verification (the gating risk)

- [x] 4.1 Build the release `Dockerfile` (musl-static → distroless) and confirm mimalloc's bundled C links and initializes; add any build flag required on the static target — built clean, **26.4 MB** distroless static image; no extra build flag needed (rquickjs `rust-alloc` avoids musl C-`malloc` interposition)
- [x] 4.2 If mimalloc cannot link static-musl, fall back per design (jemalloc / snmalloc / dynamic-musl) and record the decision — **N/A**: mimalloc linked cleanly on static-musl, no fallback needed
- [x] 4.3 Confirm the release image starts and serves `/health` + `/execute` with the allocator active — image boots (16-slot pool, bulkhead 256), `/health` → `ok`, `/execute` compute handler → HTTP 200 `data:42`
- [x] 4.4 Check container RSS in the release image is within acceptable bounds vs the pre-change baseline — **25 MiB steady-state** after warming all 16 pool slots; arenas commit lazily (modest, well within the "accept a small RSS increase" budget)

## 5. End-to-end performance verification

- [x] 5.1 Re-measure throughput through the real HTTP `/execute` path under rising concurrency (Python harness or `oha`/`wrk`), not just `host.run` — measured with `oha` against the static-musl distroless image, concurrency 1→48, on both a musl-malloc baseline image (temp allocator removal, reverted) and the mimalloc image
- [x] 5.2 Confirm the resilience spec scenarios hold: throughput scales with concurrent workers up to core count, per-core efficiency does not collapse, single-thread not regressed — **all three hold**: mimalloc scales monotonically past core count while musl plateaus at ~65–68k (the allocator becoming the ceiling); the A/B gap widens with concurrency (1.26×→2.25×, i.e. no serialization collapse); single-thread improves (8,440 vs 6,723 req/s at c=1)
- [x] 5.3 Record before/after numbers — HTTP-path A/B table recorded in `docs/design/allocator-scaling.md` (musl vs mimalloc, c=1→48); the isolated `host.run` 17× / 1.7× figures also captured there. Note: end-to-end HTTP multiplier (~2.25× at c=48) is smaller than the bench's 17× because fixed per-request machinery (HTTP/serde/fresh-Context) dominates a trivial handler — a heavy handler gave near-identical req/s, confirming the handler body is not the gate.

## 6. Documentation

- [x] 6.1 Add a `docs/design/` note capturing the measured root cause (musl malloc contention), the sweep methodology, and the before/after figures; link it from the `resilience` spec (`docs/design/allocator-scaling.md`, referenced from the resilience delta spec's "Parallel execution scaling" requirement so it folds into the main spec on sync)
- [x] 6.2 Update any stale RPS references (e.g. README/docs) that cite the old ~2.4k ceiling — grep of `README.md` + `docs/` found no in-repo citation of the old ceiling (README only names mimalloc in the tagline); the stale figure lived only in session memory, already superseded. Nothing to edit.
