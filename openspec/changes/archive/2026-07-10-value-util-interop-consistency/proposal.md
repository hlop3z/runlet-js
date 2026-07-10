## Why

A consolidation audit of the six value-utils (`decimal`, `money`, `datetime`, `text`, `list`, `dict`)
found that the `list` collection verbs mishandle `money`/`decimal` wrapper values: aggregates
silently drop `money` columns, `sort_by` orders wrappers lexically, and `group_by`/`unique` key by
amount-only or reference identity. These are real correctness bugs that contradict the docs and the
files' own claims (`list.js` header: "a currency column is summed EXACTLY"). Lock and correct the
value-util surface — one interop protocol, consistent naming, honest docs — before adding any more
utils.

## What Changes

- **Interop protocol.** Introduce a small shared interop protocol on the value-utils: each wrapper
  (`decimal`, `money`, `datetime`, `text`) exposes a canonical **numeric/ordering key** and a
  canonical **equality/group key**. The `list` shaping and aggregate verbs
  (`sum`/`avg`/`min`/`max`/`sort_by`/`unique`/`unique_by`/`group_by`/`where`) route through it
  instead of raw JS operators, so they compose correctly over wrapper values.
- **`list` aggregates preserve currency.** `sum`/`avg`/`min`/`max` over a `money` column return a
  `money` value preserving currency and **THROW on mixed currencies** (matching the existing
  `money` same-currency discipline). Over a `decimal`/number column they keep returning `decimal`.
  Previously money columns were silently skipped (`sum` → `0`, `avg`/`min`/`max` → `null`).
- **`sort_by` orders numerically** for `decimal`/`money` fields (was lexical string comparison).
- **`group_by`/`unique`/`unique_by` use the canonical keys** — currency is part of a money key
  (USD 19.99 ≠ EUR 19.99) and equal wrapper values dedupe (was reference identity).
- **`where` matches via canonical equality** for wrapper fields (was strict `!==`).
- **BREAKING: remove the three deprecated camelCase aliases** on `decimal` — `isZero`, `isNegative`,
  `toNumber` — and their `@deprecated` `.d.ts` declarations. Callers use `is_zero`/`is_negative`/
  `to_number`.
- **Surface lock (cosmetic):** rename the private `allocateWeights` helper to snake_case; settle
  `to_string` symmetry across the six utils (one rule); declare `toString`/`valueOf` on the `List`
  and `Dict` `.d.ts` interfaces (matching `Text`).
- **Docs sync:** correct `docs/13-lists-and-dicts.md` + `README.md` (money-safe sums, numeric
  `sort_by`), and document the currently-undocumented `list.unique_by`, `list.last`,
  `money.neg`/`abs`/`to_number`, and `decimal.neg`/`abs`.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `list`: aggregate/shaping verbs gain defined, correct behavior over `money`/`decimal` wrapper
  values — currency-preserving aggregates that reject mixed currencies, numeric/chronological
  ordering, and canonical grouping/dedup/matching.
- `decimal`: **remove** the deprecated camelCase aliases (`isZero`/`isNegative`/`toNumber`); the
  surface is snake_case only.

<!-- Not spec-level: money's own contract is unchanged (list detects money columns via money's
existing amount()/currency()/same-currency rule); the money/list/dict .d.ts coercion-hook
declarations and the private-helper rename are typing/implementation details handled in
design.md + tasks.md. -->


## Impact

- **Code:** `crates/runlet-core/src/js/list.js` (the verbs + `num_of`), `money.js` and `decimal.js`
  (key contract + alias removal + private-helper rename), `datetime.js`/`text.js` (key contract if
  they participate in ordering/grouping), `crates/runlet-core/src/js/base.d.ts` (alias removal +
  List/Dict coercion hooks), regenerated `container/types.d.ts`.
- **Golden test:** `types_dts_is_up_to_date` (`crates/runlet-core/src/types.rs`) must stay green
  after regenerating `container/types.d.ts`.
- **Specs:** delta specs for `list`, `money`, `decimal`, `dict` in this change.
- **Docs:** `docs/05-decimal.md`, `docs/13-lists-and-dicts.md`, `README.md` reference blocks.
- **Compatibility:** removing the deprecated camelCase aliases is technically breaking for any
  script still calling them (accepted). All other changes make previously-wrong results correct;
  scripts that relied on the buggy behavior (e.g. a money column silently summing to `0`) change
  output.
- **Constraints:** pure-JS, no Rust bridge (QuickJS supports what's needed); snake_case author
  surface; Docker-only build/test; behavior covered by the Python harness + Rust unit tests.
