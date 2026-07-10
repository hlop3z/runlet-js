## Why

The sandbox is a business-scripting language and already ships first-class value-utils for money
(`$`/`money`), exact numbers (`Decimal`), dates (`datetime`), and strings (`text`) — each a
chainable, immutable, snake_case global. The last everyday gap is **shaping collections**: arrays
of records and single record objects. Raw JS is the worst surface for exactly the audience the box
targets — ERP / e-commerce **self-serve authors who are not programmers**. Grouping orders by
status, summing a column of prices, sorting rows by a field, or safely reading a nested value all
require arrow functions, `reduce`, `Object.entries` dances, and float-summed currency. Those
authors hand-roll it (wrong, inconsistent) or give up.

Two utils — `list` (a table of records) and `dict` (one record) — close the gap with a
**field-name-first, callback-free** surface. The design is not invented: it is the settled pattern
across SQL, **Shopify Liquid** (the direct prior art — same audience, same domain, embedded
language: `products | where | sort | map | uniq | sum`), Jinja `attribute=`, and lodash's iteratee
shorthands. We adopt that vocabulary and make the shorthand the *only* form.

## What Changes

- **New always-on `list` global** — a chainable immutable value-util wrapping an array of records.
  `list([...])` wraps; transforms return new `list` (or `dict`) values; `.to_array()`/`toJSON`
  unwrap to a plain array. **Every verb takes field-name strings, never callbacks:**
  `where({field: value, ...})` (match-by-example filter), `sort_by("field", "desc"?)`,
  `group_by("field")` → returns a `dict`, `column("field")` (flat list of one column — named
  `column`, not the lodash/Rails jargon `pluck`), `unique()`/`unique_by("field")`,
  `sum`/`avg`/`min`/`max`/`count`, `first()`/`last()`, `get(i)`/`len()`.
- **New always-on `dict` global** — a chainable immutable value-util wrapping one record object.
  `get("a.b.c", default)` (safe nested read), `pick(...)`/`omit(...)`, `keys()`/`values()`/
  `entries()` (→ a `list`), `has("field")`, `merge({...})`, `to_object()`/`toJSON` unwrap.
- **Aggregates return an exact `Decimal`.** `list(orders).sum("total")` yields a `Decimal` that
  composes straight into `$`/`money`. An ERP box must never float-sum currency
  (`0.1 + 0.2`). `count()` returns a plain number; authors can `.to_number()` a non-money column.
- **`group_by` bridges `list` → `dict`; `dict.entries()` bridges `dict` → `list`.** The two are one
  matched design, so this change ships them together.
- **Zero new dependency, no Rust domain math.** Pure JS composing over `Array`/`Object`, mirroring
  the `text` value-util's structure (thin JS wrapper + a ~30-line Rust injector), reusing the
  existing `__decimal` FFI for the aggregates. The engine removes `Proxy`, so indexing is
  `.get(i)`/`.at(i)` (no transparent `wrapped[i]`); iteration is via `Symbol.iterator`.
- **Injected identically under both `Profile::Full` and `Profile::Deterministic`** — collection ops
  touch no clock, no randomness, no ambient state, so the determinism sanitizer removes nothing.
  This bans `shuffle`/`sample` from the surface (they would need `Math.random`).
- **Output/allocation guard** in the spirit of the engine's `max_*_bytes` limits and `text.js`'s
  `MAX_OUTPUT` — caller-controlled expansions (e.g. an oversize `chunk`/repeat-shaped op) are capped
  before allocation.
- **`list.d.ts` + `dict.d.ts` fragments** folded into `base.d.ts` so every method is
  IntelliSense-discoverable; the D11 golden test (`types_dts_is_up_to_date`) keeps
  `container/types.d.ts` in sync.

## Capabilities

### New Capabilities
- `list`: the chainable, immutable, field-name-first collection value-util over an array of records
  — SQL/Liquid-named shaping verbs (`where`/`sort_by`/`group_by`/`column`/`unique`), exact
  `Decimal` aggregates, always injected under both profiles, pure and deterministic, with a
  caller-controlled allocation guard. `group_by` returns a `dict`.
- `dict`: the chainable, immutable record value-util over one object — safe nested `get`,
  `pick`/`omit`, `keys`/`values`/`entries`, `has`, `merge`. `entries` returns a `list`.

### Modified Capabilities
<!-- None. `list`/`dict` are net-new always-on globals; no existing spec's requirements change. -->

## Impact

- **New code:** `crates/runlet-core/src/js/list.js` + `dict.js` (the wrappers),
  `crates/runlet-core/src/collections.rs` (one injector for both, or `list.rs`/`dict.rs`),
  `list.d.ts` + `dict.d.ts` fragments folded into `crates/runlet-core/src/js/base.d.ts`.
- **Wiring:** `engine.rs` injects `list`/`dict` alongside the other always-on value-utils, after
  `decimal` (the aggregates compose over `__decimal`); regenerate `container/types.d.ts` so the D11
  golden test passes.
- **No new dependency, no Rust math crate, no `__sys`/FFI bridge beyond the existing `__decimal`,
  no config surface, no metering.**
