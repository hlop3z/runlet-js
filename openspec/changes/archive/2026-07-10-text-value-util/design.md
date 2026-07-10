## Context

The sandbox ships value-utils for money (`$`/`money`), exact numbers (`Decimal`), and dates
(`datetime`) — each a chainable, immutable, snake_case global. String handling is the last gap:
authors fall back to raw JS, whose surface is camelCased and verbose and lacks the shaping verbs
business/ERP scripts reach for (slugify, mask, collapse, pad). This change adds a `text`
value-util in the same house style.

The structural template is `datetime` (`crates/runlet-core/src/js/datetime.js` +
`crates/runlet-core/src/datetime.rs`): a thin immutable JS wrapper plus a ~30-line Rust injector
that runs after the engine sets up globals. Unlike `datetime`, `text` needs no `__sys` bridge and
no Rust domain math — its operations are pure JS.

Constraints that shape the design:
- The repo's lint gauntlet and the "chainable instance-method-only, snake_case, no static
  shortcuts" convention (CLAUDE.md; overrides camelCase for this business-scripting surface).
- The D11 golden test (`types_dts_is_up_to_date`) requires `base.d.ts` → `container/types.d.ts`
  to stay in sync for any surface change.
- Build/test are Docker-only (aws-lc-sys needs a C toolchain; native cargo is WDAC-blocked here).

## Goals / Non-Goals

**Goals:**
- A pure, always-on `text` value-util injected identically under both profiles.
- Pythonic snake_case naming over native JS string operations (rename, don't reimplement).
- A small ERP shaping verb set (slugify/mask/collapse/truncate/pad) composed from JS primitives.
- Zero new dependency, no Rust math, no config surface, no metering.
- Full IntelliSense coverage via `base.d.ts`, kept honest by the D11 golden test.

**Non-Goals:**
- Reimplementing Unicode semantics (code-point/grapheme counting, locale-aware casing). We
  inherit JS's UTF-16 code-unit semantics and say so.
- Semantic-domain validation (`is_email`, `is_phone`, E.164 normalization) — reserved for a
  future validation util. Only character-class predicates (`is_digit`, etc.) live here.
- Reversible encoding/hashing (hex/base64/url, hmac, uuid) — that stays `$sys.crypto`/codec.
- A `str` factory name (would shadow the common `str` local in scripts) — using `text`.

## Decisions

### D1 — Strategy A: pure-JS wrapper, no Rust domain bridge, no new dependency

**Decision:** Implement the entire surface — including `slugify` — as a pure-JS wrapper over
`String.prototype`, with only a minimal Rust injector (`text.rs`) that evals the wrapper. No
`__sys`/FFI bridge, no crate for Unicode.

**Why (spike evidence):** The one operation that could have forced Rust is `slugify`'s diacritic
folding. A spike ran through the actual QuickJS engine (rquickjs 0.12) and confirmed:
`"Café Ör Ãb".normalize("NFD").replace(/[̀-ͯ]/g,"")` → `"Cafe Or Ab"`; a full slugify
pipeline → `"ac-me-01"`; and `String.prototype.normalize` is present. Casing is Unicode-**default**
and locale-independent (`"İSTANBUL".toLowerCase()` → the default `i̇…`, not the Turkish-locale
`i`), so every op is deterministic and reproducible. Nothing in the surface needs Rust.

**Alternatives considered:**
- **B — full Rust `__sys("text")` bridge** (crates like `unicode-normalization`, `slug`, `heck`):
  gives total control of Unicode behavior but adds a dependency + supply-chain review + an FFI
  round-trip per op, for zero correctness gain given the spike. Rejected.
- **C — hybrid** (pure JS for easy verbs, Rust only for slugify/normalize): the fallback if the
  spike had shown missing `normalize`. Moot now. Rejected.

### D2 — Value shape: immutable wrapper with `.value`/`toString`/`toJSON` unwrap

**Decision:** `text("...")` returns an immutable value; transforms return new `text` values;
`.value`/`toString`/`toJSON`/`valueOf` unwrap to the plain string. Mirrors `datetime`'s
immutable-value pattern (which serializes its canonical form via `toJSON`/`toString`).

**Why:** Consistency with the other value-utils and with the "logic proposes" immutability
discipline. The wrap/unwrap ceremony is pure ergonomics here (a JS string is already immutable
and correct) — accepted as the price of a uniform, IntelliSense-discoverable chainable surface,
the same trade the other value-utils make.

**Alternative considered:** a namespace of free functions (`text.slugify(s)`) — rejected: it
violates the chainable-instance-method-only convention and the "no static shortcuts" rule.
Monkey-patching `String.prototype` — rejected: mutates a builtin the sandbox otherwise keeps
pristine, and risks collisions.

### D3 — Delegate JS semantics verbatim; return types match Python intuition

**Decision:** Renames delegate 1:1 to native methods and inherit their semantics (UTF-16
code-unit counting/width). Predicates return `boolean`; `split`/`rsplit`/`splitlines` return
`string[]` (plain strings, not wrapped); `count` returns `number`; content transforms return a
`text` value so chaining continues.

**Why:** "Pythonic" here means naming, not behavior (per the user). Not reimplementing counting
keeps the util tiny and avoids a per-op `Array.from` spread. Returning plain strings from `split`
matches how a script consumes the pieces (iterate/index), while transforms stay chainable.

### D4 — Bounded output size for width/repeat

**Decision:** `zfill`/`ljust`/`rjust`/`center` (and any internal `repeat`) validate the requested
width against a fixed cap and throw a developer/script error when exceeded, before allocating.

**Why:** Width is caller-controlled; `text("x").rjust(1e9)` would OOM the isolate. This mirrors
the engine's `max_*_bytes` philosophy of failing closed on unbounded allocation. The cap is a
compile-time constant in the wrapper (no config surface needed for v1).

### D5 — Injected under both profiles; no determinism sanitizer entry

**Decision:** `engine.rs` injects `text` alongside the always-on value-utils, under both `Full`
and `Deterministic`. Nothing is added to `js/determinism.js`.

**Why:** Text ops touch no clock, no randomness, no ambient authority (contrast `datetime.now`,
which the sanitizer deletes). There is simply nothing to remove — this is the simplest value-util
in the repo on that axis.

## Build-vs-Adopt Decisions

### Decision: Unicode normalization / slugify — Build (hand-written pure-JS)

- **Status**: approved
- **Why**: The spike proved QuickJS's built-in `normalize("NFD")` folds diacritics correctly with zero dependency; the sandbox is a pure-JS wrapper, so adopting a Rust crate would *add* an `__sys` FFI bridge + serialization + supply-chain review — more build surface, not less — for behavior we already have.
- **Considered**: `slug`/`deunicode` via a Rust bridge (mature ~21M downloads, adds transliteration — but that's out of scope for ERP slugs and costs a dep+bridge); `unicode-normalization` only (exactly what QuickJS's `normalize` already provides — pure ceremony).
- **Scope note**: Non-latin scripts are *dropped*, not transliterated. ERP slugs (SKUs/reference codes) want predictable diacritic-folding; silent CJK/Cyrillic romanization is a deliberate non-goal. Revisit by adopting `deunicode` behind the bridge if transliteration is ever required.
- **Isolation**: entirely inside `js/text.js` — swappable to a Rust bridge later without touching `specs/` or `config`.

### Decision: Output-size / OOM guard — Build (inline invariant)

- **Status**: approved
- **Why**: No tool to adopt; capping caller-controlled width/repeat before allocation is a one-line invariant, the same shape as the engine's existing `max_*_bytes` guards.
- **Considered**: no external option applies.
- **Isolation**: a compile-time constant + guard inside `js/text.js` (D4); no config surface for v1.

## Risks / Trade-offs

- **[QuickJS Unicode data changes across engine bumps]** → A future rquickjs bump could alter
  `normalize`/casing behavior. Mitigation: the golden slugify/case scenarios in the spec become
  unit tests, so a regression is caught in CI, not in production.
- **[UTF-16 code-unit semantics surprise authors]** → `count`/width on emoji/CJK differ from
  Python's code-point intuition. Mitigation: documented explicitly in the `.d.ts` and beginner
  docs as "JS-native semantics"; we do not silently promise otherwise.
- **[Masking mistaken for security]** → `mask`/`redact` is lossy *display*, not encryption.
  Mitigation: doc wording and its placement in `text` (shaping), deliberately separate from
  `$sys.crypto`.
- **[Scope creep toward validation]** → pressure to add `is_email` etc. Mitigation: the spec
  draws the character-class vs semantic-domain line; validation is an explicit non-goal reserved
  for a separate util.

## Migration Plan

Additive, no breaking changes. Deploy adds one always-on global; a script's own `text` local
shadows it harmlessly. Rollback is removing the injector call. No data or config migration.

## Open Questions

- **Exact output-size cap value** — reuse/derive from an existing engine limit vs. a fresh
  constant. Lean: a standalone constant in the wrapper for v1; revisit if authors hit it.
- **`title` semantics** — native has no title-case; define as capitalize-each-whitespace-word.
  Confirm that's the ERP-useful behavior (vs. leaving `title` out and shipping only `capitalize`).
- **Ellipsis marker for `truncate`** — single char `"…"` vs. `"..."`, and whether the marker
  counts toward `limit`. Lean: `"…"`, counted toward `limit` (Python-ish `textwrap` feel).
