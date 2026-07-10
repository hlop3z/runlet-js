## 1. Rounding-mode vocabulary (Decimal core)

- [x] 1.1 Add a `mode` parser mapping the snake_case vocabulary (`half_up`, `half_even`, `up`, `down`, `ceil`, `floor`) onto `rust_decimal::RoundingStrategy`; unknown mode → catchable error
- [x] 1.2 Extend the `__decimal` FFI to carry a `mode` argument alongside `places` (pack into `rhs` or widen the call), keeping the default `half_up` for backward-compat
- [x] 1.3 Implement `round(places, mode)` and `round_to(step, mode)` in `decimal.rs`; `round_to` rounds to the nearest multiple of `step`
- [x] 1.4 Rust unit tests: `half_even` ties-to-even (2.5→2, 3.5→4), `half_up` default unchanged, `round_to("0.05")`, unknown-mode error

## 2. Bounded scalar helpers (Decimal wrapper, JS-only)

- [x] 2.1 Add `clamp(lo, hi)`, `min(other)`, `max(other)`, `pct(p)` to the Decimal JS wrapper, composed over existing `cmp`/`mul`/`div` (no Rust)
- [x] 2.2 Unit tests for clamp/min/max/pct edge cases (below/above range, equal bounds)

## 3. snake_case migration (Decimal)

- [x] 3.1 Rename Decimal wrapper methods to snake_case (`is_zero`, `is_negative`, add `is_positive`, `to_number`); keep `toString`/`toJSON`/`valueOf` JS-spelled
- [x] 3.2 Keep camelCase spellings (`isZero`, `isNegative`, `toNumber`) as deprecated aliases delegating to the snake_case forms
- [x] 3.3 Update the `decimal` spec's example call sites (`$(...)` → `Decimal(...)`) are reflected in tests

## 4. ISO 4217 exponent table

- [x] 4.1 Add the static currency → minor-unit-exponent table (JS lookup object in the wrapper, per the design's leaning), covering the ISO 4217 code set; unknown code → catchable error
- [x] 4.2 Helper to resolve a currency's decimal places and its `10^exponent` scale factor
- [x] 4.3 Tests: USD=2, JPY=0, BHD=3, CLF=4, unknown-code error

## 5. Currency cascade + config

- [x] 5.1 Add `default_currency` to the server `Config` (`crates/runlet/src/config.rs`) and `currency` to the per-request config; thread the resolved default into the engine/context
- [x] 5.2 Implement the three-level resolution (explicit arg → `config.currency` → `default_currency` → error) at money construction
- [x] 5.3 Tests for each cascade level and the no-currency-resolvable error

## 6. Money value core (`$` / `money`)

- [x] 6.1 Add a `money` wrapper (new `src/js/money.js`) constructed from amount + resolved currency; `$` and `money` are the same constructor, injected always-on
- [x] 6.2 Currency-safe arithmetic: `add`/`sub` (same-currency, else error), `mul` (scalar only, money×money → error), `div` (scalar → money; same-currency money → `Decimal` ratio; cross-currency → error), `neg`, `abs`; no implicit FX
- [x] 6.3 Business percentages: `pct(p)`, `add_pct(p)`, `sub_pct(p)`, each rounded to the currency precision
- [x] 6.4 Currency-aware `round(mode?)` rounding to the currency minor unit (default `half_up`)
- [x] 6.5 Currency-safe comparison: `cmp`/`eq`/`lt`/`lte`/`gt`/`gte` (same-currency, else error), `is_zero`/`is_negative`/`is_positive`

## 7. Largest-remainder allocation

- [x] 7.1 Implement the Hamilton allocator in `decimal.rs`: floor each share to the currency minor unit, distribute leftover units to the largest fractional remainders, order-stable tie-break
- [x] 7.2 Extend the FFI with an array-return envelope (`{list:[...]}`) for allocate; wrap each element back into a money value
- [x] 7.3 JS `allocate(weights)`, `allocate_to(n)`, and `split(n)` (alias) on the money wrapper
- [x] 7.4 Rust unit tests: `allocate_to(3)` of 100.00→[33.34,33.33,33.33], `allocate([70,30])` of 0.05→[0.04,0.01], JPY whole-unit split, determinism (same input → same output), Σparts == whole, share-count preserved incl. zero weights

## 8. Money serialization + interop

- [x] 8.1 `to_json` returns `{ amount, currency, minor_units }` always; `minor_units` an integer derived from the currency exponent
- [x] 8.2 `to_minor()` (integer minor units, zero-decimal-correct), `amount()` (→ Decimal), `currency()`, `format()` (human `"$19.99"`), `to_string()`
- [x] 8.3 Tests: serialization shape, `minor_units` for JPY (no ×100), `to_minor` for a payment-API amount, `amount()` returns a currency-less Decimal

## 9. Types, docs, conventions

- [x] 9.1 Update `container/types.d.ts` with the `money` (`$`) and reframed `Decimal` surfaces, all snake_case; keep the D11 golden test `types_dts_is_up_to_date` green
- [x] 9.2 Update beginner docs (`docs/05-decimal.md` → money + decimal split) and the README reference
- [x] 9.3 Correct the stale CLAUDE.md conventions bullet (value-utils are snake_case, per memory `business-scripting-snake-case`)
- [x] 9.4 Write the migration note: `$`-as-decimal → `Decimal`; `toCents(places)` → `to_minor()`

## 10. Verification

- [x] 10.1 Run the golden `types_dts_is_up_to_date` test and full `cargo test` (Docker, per WDAC constraint)
- [x] 10.2 Run `task clippy` (plain `cargo clippy`) until clean — re-run to surface cascading lint errors
- [x] 10.3 Add a Python harness section (or extend `tests/test_simple.py`) exercising a real invoice-with-tax and a refund split end-to-end through `/execute`
