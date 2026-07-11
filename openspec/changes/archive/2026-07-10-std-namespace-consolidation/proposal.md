## Why

The sandbox injects ~13 bare globals (`money`, `Decimal`, `datetime`, `text`, `list`,
`dict`, `io`, `http`, `s3`, `json`, `log`, `emit`, `$`) plus a second namespace object
`$sys`. There is no single, discoverable "here is everything the box gives you" surface,
and the two-namespace split (`$sys` vs. bare globals) is arbitrary. Authors can't
`import`-style destructure a curated set, and editor autocomplete has no one root to
explore. With **no current users**, this is the moment to consolidate the author-facing
surface into one canonical namespace before it accretes more members (the incoming
`template`/`unit`/`csv`/`tax`/`pricing` utils).

## What Changes

- **BREAKING**: Introduce a single canonical namespace object **`$std`** that holds
  **every** built-in — utils (`money`, `decimal`, `text`, `datetime`, `list`, `dict`),
  capabilities (`io`, `http`, `s3`), the former `$sys` members (`crypto` kept grouped;
  `env`/`secrets` hoisted), and the channels (`json`, `log`, `emit`). Everything is
  **defined once** inside `$std`.
- **BREAKING**: **Delete `$sys`.** `$sys.crypto.*` → `$std.crypto.*` (grouped, unchanged
  shape); `$sys.env` → `$std.env`; `$sys.secrets` → `$std.secrets`.
- **BREAKING**: The bare globals become a **projection of `$std`**, not independent
  definitions. A single declarative `EXPOSE` list re-mirrors a curated subset up to
  `globalThis`; each mirrored global is the *same object reference* as its `$std` member
  (`$ === $std.money`, `log === $std.log`). After this change the only globals are:
  `$std`, `$` (money shortcut), `json`, `log`, `emit`. The bare `money`, `Decimal`,
  `datetime`, `text`, `list`, `dict`, `io`, `http`, `s3` globals are **removed** — reached
  only via `$std` (or `$` for money).
- **Invariant**: only **pure** members are eligible for global exposure; prunable ambient
  authorities (`datetime.now`, `crypto.uuid`, `Math.random`, `Date()`) live only at their
  `$std` path and are never mirrored, so the determinism prune cannot be defeated by a
  surviving global reference.
- `$std` is **deep-frozen** after the determinism prune, and the mirrored global bindings
  are locked non-writable — the surface is tamper-proof for the running handler.
- **Typing**: one `interface Std` + `declare const $std: Std` becomes the single source of
  truth; the mirrored-global type declarations are derived from it so `$std.io` and
  `const { io, http } = $std` both type-check off the same interface and cannot drift.
- **HARD CUTOVER** — no backwards-compat aliases, no deprecation window (consistent with
  the prior camelCase-alias removal).
- **Out of scope** (own follow-on change): the minijinja-backed `$std.template` util and
  the new `unit`/`csv`/`tax`/`pricing`/`check` utils. This change is dependency-free and
  purely a surface refactor.

## Capabilities

### New Capabilities
- `std-namespace`: The `$std` canonical namespace and the global-projection model — what
  `$std` contains, how the `EXPOSE` list mirrors a curated subset to `globalThis`, the
  pure-members-only exposure invariant, the deep-freeze/lock and prune-before-freeze
  ordering, and the single-source typing contract (both `$std.*` and destructuring typed
  off one interface; golden-tested).

### Modified Capabilities
- `sys`: `$sys` is removed. The crypto/codec surface relocates to `$std.crypto.*`
  (grouped, otherwise unchanged); `env`/`secrets` relocate to `$std.env`/`$std.secrets`.
  Semantics (opaque secret handles, HMAC-only sinks, config-gated population) are
  unchanged — only the access path and the enclosing namespace change.

## Impact

- **JS injection**: every `crates/runlet-core/src/js/*.js` IIFE (`money`, `decimal`,
  `text`, `datetime`, `list`, `dict`, `io`, `http`, `s3`, `log`, `sys`) rewritten to
  populate `$std` instead of `globalThis`; internal cross-references (`list.js`,
  `money.js`, `dict.js`) repointed; `sys.js`'s `$sys` assembly removed; a new `EXPOSE`
  projection + freeze/lock epilogue added; `determinism.js` prunes `$std.datetime.now` /
  `$std.crypto.uuid`.
- **Engine**: `engine.rs::inject_apis` and the injection ordering updated so capabilities
  land under `$std` and the projection runs before user code; deep-freeze/lock step added
  after the determinism pass.
- **Types**: `crates/runlet-core/src/js/base.d.ts` rewritten (one `Std` interface + derived
  mirror declares; old bare `declare const`s and the `Sys` interface removed);
  `container/types.d.ts` golden regenerated (D11 test `types_dts_is_up_to_date`).
- **Docs/tests**: `docs/*.md`, `README.md`, affected `openspec/specs/*`, and
  `tests/scripts/*.js` swept for bare-global / `$sys` references.
- **Dependencies**: none added or removed.
