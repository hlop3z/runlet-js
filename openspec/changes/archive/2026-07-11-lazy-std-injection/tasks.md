## 1. Baseline & validate the "eager natives, lazy wrappers" split (D2)

- [x] 1.1 Re-run `mux/baseline_0_calls` on the current tree to capture the 4.77 ms baseline for this branch (thin-LTO release, Docker). → **~5.02 ms** (range [4.91, 5.14]) on this branch/machine.
- [x] 1.2 Add a throwaway bench arm (or one-off measurement) that registers only the native bridges (`__decimal`, `__sys`, `__template`, `__default_currency`, `__sys` secrets) without eval'ing the wrapper IIFEs, to confirm native registration is a small fraction of 4.77 ms. Record the number in `design.md` (D2 validation); if it is *not* small, revise D2 before proceeding. → **~572 µs** natives-only (~11% of baseline); D2 confirmed, recorded in design.md.

## 2. Lazy accessor mechanism for `$std` members (D1)

- [x] 2.1 Split each per-util `inject_*` (`decimal`, `money`, `sys`, `datetime`, `text`, `collections`, `template`, `check`) into (a) eager native-bridge registration and (b) a wrapper-build closure that evals the wrapper IIFE and returns the built member. Keep the wrapper source (`include_str!`) untouched. → `register_native` (decimal/sys/template) + `pub(crate)` wrapper consts; the engine assembles per-unit shadow sources.
- [x] 2.2 In `engine.rs::run`, replace the eager wrapper evals with installation of non-configurable, getter-only accessors on `$std` (`Object.defineProperty($std, name, { get, enumerable: true, configurable: false })`), where the getter builds → deep-freezes → memoizes → returns the member on first access. → `js/std_lazy.js` installs the accessors; native `__stdBuild(key)` parses+builds via a shadow realm; getter deep-freezes + memoizes.
- [x] 2.3 Register the eager native bridges + per-request scalars (`__default_currency`, `__sys` secrets) up front so any member's wrapper can resolve its native dependency without forcing another member's build. → `inject_lazy_std` registers `__decimal`/`__sys`/`__template`/`__default_currency`/secrets eagerly.
- [x] 2.4 Verify inter-member dependencies resolve through the accessor path (money → decimal, list/dict → decimal, datetime → sys) — a wrapper that needs another *wrapper* reads it via `$std.<dep>`. → the build scratch's prototype is the real `$std`, so a wrapper's `$std.<dep>` read fires that dep's lazy getter (validated by the probe: money's build built decimal exactly once).

## 3. Determinism-aware builder (D4)

- [x] 3.1 Parameterize the per-member builder by `Profile`; under `Profile::Deterministic` build the already-pruned variant (no `datetime.now`, no `crypto.uuid`) on first access. → `build_unit_sources` appends `delete $std.datetime.now`/`delete $std.crypto.uuid` to the sys/datetime shadow bodies when deterministic, before the return + freeze.
- [x] 3.2 Move the `$std.*`-member prunes out of the post-hoc `js/determinism.js` pass into the builders; keep a residual global sanitizer only for JS builtins that are not `$std` members (`Math.random`, no-arg `Date()`/`new Date()`), and confirm that split (Open Question). → **Confirmed**: `determinism.js` now only neutralizes `Math.random` + no-arg `Date()`; the `$std.*` prunes moved into the lazy builder (so the post-hoc pass never force-builds a member).
- [x] 3.3 Ensure freeze applies to the pruned object (prune-before-freeze holds within each lazy build). → the getter deep-freezes the value returned by `__stdBuild`, which already had `.now`/`.uuid` deleted; e2e test `deterministic_build_omits_ambient_authorities` asserts absence.

## 4. Lazy projected globals + identity (D3)

- [x] 4.1 In `project_std_globals` / `js/std_project.js`, install `$` as a getter that returns `$std.money` (lazy) rather than an eager `globalThis.$ = $std.money` read; keep `json`/`log`/`emit` eager. → done in `js/std_project.js`.
- [x] 4.2 Lock the exposed global bindings non-writable before `handler` runs regardless of whether the backing member has materialized. → `$` installed getter-only non-configurable at projection; `json`/`log`/`emit` re-pinned non-writable by `js/std_freeze.js`. E2e `exposed_globals_cannot_be_reassigned` asserts it.
- [x] 4.3 Confirm `$ === $std.money` (single memoized instance) via both access orders. → e2e `identity_freeze_and_container_lock` + probe assert identity.

## 5. Freeze / container locking (MODIFIED freeze requirement)

- [x] 5.1 In `freeze_std` / `js/std_freeze.js`, replace the one-shot deep-freeze-before-handler with: freeze each member at materialization (in its getter), and lock the `$std` container (`Object.freeze($std)` → non-extensible; slots already non-configurable getter-only from §2.2). → `js/std_freeze.js` locks the container + deep-freezes only eager DATA members (skips accessor slots so it never force-builds); each lazy member deep-frozen in its getter.
- [x] 5.2 Verify a handler cannot add (`$std.newThing = 1`), delete (`delete $std.io`), replace (`$std.io = fn`), or mutate a built member (`$std.money.round = 5`). → e2e `identity_freeze_and_container_lock` asserts add/delete/replace/mutate are all no-ops.

## 6. Tests

- [x] 6.1 Unit tests (in `engine.rs` / per-util modules): touched-vs-untouched build, at-most-once per request, identity (`$ === $std.money`), member frozen before handler can mutate, container locked. → `engine::tests::lazy_std_accessor_mechanism` (at-most-once + untouched-not-built via build counters) + `engine::lazy_std_tests::*` (identity/freeze/lock/reachability e2e).
- [x] 6.2 Determinism scenarios: deterministic lazy build omits `datetime.now`/`crypto.uuid`; full profile keeps them; `Math.random`/`Date()` still neutralized under Deterministic. → `deterministic_build_omits_ambient_authorities` + `full_build_keeps_ambient_authorities`.
- [x] 6.3 Confirm the existing `std-namespace` scenarios still pass (reachability, destructuring forces build, gating of `io`/`http`/`s3`, template as namespace-only). → full `cargo test` green (225 tests across the workspace incl. `--no-default-features`); reachability + destructuring covered by `all_members_reachable_and_destructuring_builds`.
- [x] 6.4 Confirm the D11 golden test `types_dts_is_up_to_date` still passes (no `.d.ts` change expected). → passes (no `.d.ts` change).
- [x] 6.5 Run the Python integration suite (`tests/test_simple.py`) — box-only, self-contained. → all `$std` value-util sections pass end-to-end through the real HTTP box (money/decimal/datetime/template/check + **list interop**, which exercises call-time dep resolution: `list.sum` reads `$std.decimal`/`$std.money`, firing their lazy getters); sandbox/errors/meta/batch pass. Only `test_http_api` fails — the box runs inside the build container and reaches `localhost:8095` (its own loopback) rather than the host's httpbin (`status:0`); environmental, and `http` is an eager primitive untouched by this change.

## 7. Verification & quality gate

- [x] 7.1 Re-run `mux/baseline_0_calls`: expect a util-free handler ≈ 0.8 ms (~6× vs 4.77 ms); record per-member deltas for a handler touching one util. → **util-free 5.02 ms → 1.10 ms (~4.6×)**; `one_util` (money+decimal) **2.81 ms** (~1.8× vs old eager); `all_utils` (every member) **6.85 ms** (the bounded worst case — ~36% over old eager, the inherent per-build tax paid only when everything is touched). New bench arms `one_util`/`all_utils` added to `mux.rs`.
- [ ] 7.2 Re-run a k6 single-node RPS check; confirm the ceiling moved above ~2,400 RPS and note the new bottleneck (engine vs HTTP/tokio). → **Deferred** (needs a running server + k6 harness, not available in this Docker loop). The ~4.6× drop in per-request engine cost for util-free handlers is the RPS lever; the microbench stands in for it here.
- [x] 7.3 `task clippy` clean (build does not run clippy) and `cargo fmt --all --check` clean before pushing. → both clean.
- [x] 7.4 Update `docs/design/composable-core.md` (or the relevant design doc) with the lazy-materialization note and link the change; keep `docs/` capability guides accurate (no user-facing API change). → added a "Lazy `$std` materialization" section + updated the deterministic-removal split; fixed stale doc comments in `engine.rs`/`datetime.rs`/`sys.rs` that claimed `determinism.js` deletes the `$std.*` members. No user-facing API/`.d.ts` change, so the `docs/` capability guides are unchanged.
