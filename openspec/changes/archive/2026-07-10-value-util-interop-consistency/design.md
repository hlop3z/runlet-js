## Context

The six value-utils (`decimal`, `money`, `datetime`, `text`, `list`, `dict`) are pure-JS globals
injected into every QuickJS context (`crates/runlet-core/src/js/*.js`), with their typed surface in
`base.d.ts` (assembled into `container/types.d.ts`, guarded by the `types_dts_is_up_to_date` golden
test at `crates/runlet-core/src/types.rs`). A consolidation audit (see proposal) found that the
`list` shaping/aggregate verbs handle only raw scalars: they treat wrapper values by JavaScript
reference identity or default string coercion, producing wrong results —

- `num_of` (`list.js:42-53`) recognizes `Decimal`, `number`, numeric `string`; a `money` object
  falls through to `null` and is **silently skipped** by `sum/avg/min/max`.
- `sort_by` (`list.js:97`) compares with `<`/`>`; wrappers coerce via `toString` → **lexical** order.
- `group_by` (`list.js:141`) keys by `String(field)`; `String(money)` is amount-only → **currency
  dropped**.
- `unique`/`unique_by` (`list.js:118-137`) dedupe via a `Set` on the raw value → **reference
  identity** for wrappers.
- `where` (`list.js:88`) uses `!==` → fails against a wrapper field by type/reference.

Constraints: pure-JS (no Rust bridge; QuickJS already provides exact `Decimal`/`money` via their
globals), snake_case author surface, and the golden test must stay green. Build/test is Docker-only
(WDAC blocks native cargo on this host).

## Goals / Non-Goals

**Goals:**
- One shared interop mechanism the `list` verbs route through, so every verb composes correctly over
  every wrapper — no per-verb, per-type special-casing scattered across the file.
- Currency-preserving `money` aggregates that reject mixed currencies.
- Lock the surface: remove the deprecated `decimal` camelCase aliases, settle `to_string` symmetry,
  and close the `.d.ts` coercion-hook gaps — all with the golden test green.
- Docs tell the truth about what the verbs now do.

**Non-Goals:**
- No new value-util and no new author-facing verbs (the `list` method set is unchanged; only its
  behavior over wrappers is corrected).
- No Rust changes beyond regenerating `container/types.d.ts` (the JS strings are `include_str!`'d).
- Not resolving the flagged naming asymmetries `len`-vs-`count` and `list.get`-vs-`dict.get`
  (recorded as an open question; changing them is a separate, larger surface decision).

## Decisions

### D1 — Two hidden interop hooks on each wrapper, consumed by `list` (Build)

Each wrapper prototype exposes two non-enumerable, underscore-prefixed hooks that `list` reads:

- `__order_key()` → a value usable for **numeric/chronological ordering and aggregation**. `decimal`
  and `money` return their exact `Decimal` (money strips currency for ordering only); `datetime`
  returns epoch milliseconds; `text` returns its string; scalars have no hook and use their natural
  JS value.
- `__id_key()` → a **stable string** for equality/grouping/dedup that fully distinguishes values.
  `money` returns `"<amount> <CURRENCY>"` (currency included); `decimal` returns its canonical
  string; `datetime` returns its ISO string; `text` returns its string.

`list` gains one internal resolver pair — `order_of(v)` (prefers `__order_key`, falls back to the
existing `num_of` numeric coercion, else the raw value) and `id_of(v)` (prefers `__id_key`, else
`String(v)` for scalars). The verbs route through them: `sort_by`/`min`/`max` compare `order_of`;
`sum`/`avg` fold `order_of` as exact `Decimal`; `group_by`/`unique`/`unique_by`/`where` key/match on
`id_of`.

**Why over alternatives:** (a) *Per-verb `instanceof` ladders* — rejected: N verbs × M wrapper types
of duplicated, drift-prone branching, exactly the smell the audit found. (b) *A public `valueOf` on
each wrapper* — rejected: `valueOf` returning a number would reintroduce float precision loss (the
whole reason `Decimal` exists) and would silently change `==`/`+` semantics in user scripts. (c)
*Rust-side comparison* — rejected: unnecessary; the data is already in-engine and the wrappers
already hold exact values. Two purpose-built hooks keep ordering (exact `Decimal`) and identity
(currency-aware string) as **distinct** concerns, which a single hook conflates.

### D2 — Money aggregates return money; mixed currencies throw (Build, reuse money's rule)

`sum`/`min`/`max`/`avg` detect a money column (first aggregatable value is a `money`) and return a
`money`, reusing money's existing same-currency guard (`sameCurrency`, `money.js`) so mixing
currencies throws the same catchable error money arithmetic already throws. Decimal/number columns
keep returning `Decimal`. **Why:** silently coercing money to a bare `Decimal` (dropping currency)
was a data-loss bug; throwing on mixed currency matches the discipline `money.add`/`sub` already
enforce, so authors get one consistent rule.

### D3 — `to_string` symmetry: keep, do not spread (Decision)

Only `money` and `text` expose a snake_case `to_string`. Rather than add it to the other four, the
canonical readout stays the JS `toString` protocol hook (present on all six) plus `String(x)`, which
the docs already teach. `money.to_string` and `text.to_string` are **retained** (they are documented,
shipped, and non-deprecated — removing them is a gratuitous break), but no new `to_string` twins are
added and none is added to `decimal`/`datetime`/`list`/`dict`. **Why:** "one canonical form" is best
served by the universal protocol hook; minting four more redundant twins widens the surface the rule
is trying to shrink. This asymmetry is documented as intentional.

### D4 — `.d.ts` coverage: declare the coercion hooks that already exist (Adopt the Text precedent)

`List` and `Dict` implement `toString`/`valueOf` but their `base.d.ts` interfaces omit them, while
`Text` declares both. Add the two lines to each interface to match the `Text` precedent, then
regenerate `container/types.d.ts` so the golden test passes. The new `__order_key`/`__id_key` hooks
are **internal** (underscore-prefixed, not author-facing) and are deliberately **not** declared in
`.d.ts` — keeping them out of IntelliSense is correct.

### D5 — Private helper rename (mechanical)

Rename `allocateWeights` (`money.js:136`) → `allocate_weights` for internal snake_case consistency.
Purely local; no surface or behavior change.

## Build-vs-Adopt Gate

### Decision: Interop dispatch over wrapper values — Build hand-written hooks + resolver

- **Status**: approved
- **Why**: The keying logic (map a `money`/`decimal`/`datetime`/`text` to an ordering value and an
  identity string) is bespoke to our in-engine wrappers and must be hand-written regardless — a JS
  collection lib only wraps `Array` calls we already have and still needs a custom iteratee that *is*
  this logic. See D1 for the two-hook shape.
- **Considered**: Adopt lodash / es-toolkit / Remeda `groupBy`/`uniqBy`/`sortBy` — rejected: they
  fall back to JS relational operators (the string-coercion bug) unless handed a custom iteratee, and
  cannot be pulled into the self-contained QuickJS engine (no npm module resolution; cargo-vet-locked
  supply chain). Per-verb `instanceof` ladders — rejected (drift-prone, the smell the audit found).
- **Isolation**: `__order_key()`/`__id_key()` on each wrapper prototype (internal, not in `.d.ts`) +
  the `order_of`/`id_of` resolver pair private to `list.js`; the verbs never branch on type directly.

### Decision: Exact numeric aggregation + currency guard — Adopt (reuse) in-repo primitives

- **Status**: approved
- **Why**: Exactness and currency safety are already solved by audited primitives — the
  `rust_decimal`-backed `Decimal` global for exact folds and `money.js` `sameCurrency` for the
  mixed-currency throw. Hand-writing new math would duplicate them and reintroduce precision/currency
  risk. See D2.
- **Considered**: Build fresh decimal summation/comparison/currency checks in `list.js` — rejected as
  gratuitous duplication of correctness-critical code.
- **Isolation**: aggregates fold through the injected `Decimal`/`money` globals; the mixed-currency
  error path is money's existing `sameCurrency` guard, not a new check in `list`.

## Risks / Trade-offs

- **Removing the camelCase aliases is breaking** → Accepted per the scope decision; migration is a
  mechanical rename documented in the decimal delta spec. The snake_case forms have shipped since the
  aliases were introduced, so any current caller already has the target available.
- **Behavior change for money columns that previously summed to `0`** → A script that (wrongly)
  relied on a money column silently aggregating to `0`/`null` will now get a correct money value or a
  thrown mixed-currency error. This is the intended fix, but it *is* an output change; called out in
  the proposal's Impact and covered by new harness cases.
- **`__order_key`/`__id_key` add two methods to hot value objects** → Negligible: they are cheap
  accessors returning already-computed fields, defined once on the prototype, and only ever called by
  `list` verbs, not per value construction.
- **`order_of` fallback ambiguity for mixed-type columns** → Spec marks mixed incomparable columns
  "unspecified but SHALL NOT panic"; the resolver returns a stable-but-unspecified order rather than
  throwing, matching the existing skip-non-numeric leniency.

## Migration Plan

1. Land the JS changes (hooks on `decimal`/`money`/`datetime`/`text`; resolver + verb rewiring in
   `list.js`; alias removal in `decimal.js`; `allocate_weights` rename in `money.js`).
2. Update `base.d.ts` (drop the three `@deprecated` alias lines; add `List`/`Dict` coercion-hook
   declarations) and regenerate `container/types.d.ts`; confirm `types_dts_is_up_to_date` passes.
3. Sync docs (`docs/05-decimal.md`, `docs/13-lists-and-dicts.md`, `README.md`).
4. Verify in Docker: `cargo test` (unit + golden), `task clippy`, then the Python harness
   (`tests/test_simple.py`) with new value-util interop cases.

Rollback: the change is additive-plus-one-removal in pure-JS strings; reverting the commit restores
the prior behavior with no data-migration concern.

## Open Questions

- **`len` vs `count` on `list`, and `get` (positional on `list`) vs `get` (dotted-path on `dict`).**
  Flagged by the audit as cross-util naming friction. Not resolved here — collapsing `len`/`count` or
  renaming a `get` is a wider author-surface break that deserves its own change. Recorded so it is not
  lost.
