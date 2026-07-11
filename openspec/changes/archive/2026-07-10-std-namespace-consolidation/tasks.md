## 1. Namespace bootstrap & projection (engine)

- [x] 1.1 Add a `$std` bootstrap (`globalThis.$std = {}`) as the first injected script, before any util/capability IIFE runs
- [x] 1.2 Define the single `EXPOSE` list (`{ $: "money", json, log, emit }`) and a projection epilogue that sets `globalThis[g] = $std[member]` for each entry, running BEFORE the user script evals
- [x] 1.3 Add the freeze/lock epilogue: deep-freeze `$std` and lock each EXPOSE'd global binding non-writable/non-configurable, sequenced AFTER the determinism prune and BEFORE `handler(ctx)`
- [x] 1.4 Update `engine.rs` injection ordering (`inject_apis` and the surrounding sequence) so capabilities land under `$std`, and the bootstrap/project/prune/freeze steps run in the D3 order

## 2. Migrate util IIFEs to `$std`

- [x] 2.1 `decimal.js`: assign `$std.decimal` instead of `globalThis.Decimal`
- [x] 2.2 `money.js`: assign `$std.money`; repoint its load-time `Decimal` reference to `$std.decimal` (load-order: decimal before money)
- [x] 2.3 `text.js`: assign `$std.text`
- [x] 2.4 `datetime.js`: assign `$std.datetime`
- [x] 2.5 `list.js`: assign `$std.list`; repoint internal `Decimal`/`money`/`dict` references to `$std.*` per the D2 capture-vs-live rule (pure utils may be captured at load; nothing here touches a prunable authority)
- [x] 2.6 `dict.js`: assign `$std.dict`; repoint internal `list` reference to `$std.list`
- [x] 2.7 Verify no residual `globalThis.<util>` writes or bare-global cross-references remain across the util files

## 3. Fold `$sys` into `$std` and update determinism

- [x] 3.1 `sys.js`: remove the `$sys` assembly; populate `$std.crypto` (grouped), `$std.env`, `$std.secrets`; keep the `__sysMakeSecrets` hook and secret-handle semantics intact
- [x] 3.2 `determinism.js`: prune `$std.datetime.now` and `$std.crypto.uuid` (was `datetime.now` / `$sys.crypto.uuid`); keep `Math.random` deletion and `Date()` replacement
- [x] 3.3 Confirm the exposure list contains no prunable authority, and add/extend a determinism test asserting `$std.datetime.now`, `$std.crypto.uuid`, `Math.random`, and no-arg `Date()` are unreachable via every path after prune

## 4. Migrate capability IIFEs to `$std`

- [x] 4.1 `io.js`: assign `$std.io`
- [x] 4.2 `http.js`: assign `$std.http`
- [x] 4.3 `s3.js`: assign `$std.s3`
- [x] 4.4 Confirm profile/config gating still yields `$std.<cap> === undefined` when a capability is not configured or under `Profile::Deterministic`

## 5. Types: single-source `$std` + derived global mirrors

- [x] 5.1 Rewrite `base.d.ts`: add `interface Std { … }` and `declare const $std: Std`; remove the old bare `declare const money/Decimal/datetime/text/list/dict/io/http/s3` and the `Sys` interface
- [x] 5.2 Add the derived mirror declares (`declare const $: Std["money"]`, `json`, `log`, `emit`) generated from the same `EXPOSE` source so they cannot drift
- [x] 5.3 Regenerate `container/types.d.ts` and confirm the `types_dts_is_up_to_date` (D11) golden test passes
- [x] 5.4 Sanity-check editor typing: `$std.io`, `const { io } = $std`, and `$(...)` all resolve; `$std.nope` is a type error

## 6. Docs & tests sweep

- [x] 6.1 Update `docs/*.md` and `README.md` examples from bare globals / `$sys.*` to `$std.*` (and `$`/`json`/`log`/`emit` where they stay global)
- [x] 6.2 Update `tests/scripts/*.js` and any Python-harness expectations that reference bare util globals or `$sys`
- [x] 6.3 Update any `openspec/specs/*` example call syntax that referenced bare globals or `$sys` (non-normative text)

## 7. Verify

- [x] 7.1 Docker: `cargo test` (unit + golden) green
- [x] 7.2 Docker: `cargo clippy` clean (project gate); `cargo fmt --all --check` clean
- [x] 7.3 Run the Python integration harness (box-only) and confirm the `$std`-based scripts execute end-to-end
- [x] 7.4 Manual check under both profiles: `Profile::Full` exposes the full `$std`; `Profile::Deterministic` has io/http/s3 absent and the prunable authorities unreachable
