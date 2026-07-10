## Why

The box is positioned as a **simpler, business-made scripting language** — a Python-like feel
for non-developers — aimed at day-to-day office / e-commerce / ERP numeric work. But today the
`$` / `Decimal` global is a single generic exact-decimal: it has no notion of currency, forces
authors to hand-track decimal places and rounding modes, and offers no penny-safe way to split
an amount. Those are exactly the parts of money math that are error-prone and compliance-sensitive
(VAT rounding, refund/tax splits, per-currency minor units). The result is developer-shaped
plumbing where the audience wants a tool that reads like plain business language and is correct by
default. We fix that by adopting the *established* money standards (ISO 4217, the industry rounding
vocabulary, the Fowler/largest-remainder allocation pattern) and hiding them behind a dead-simple
snake_case API.

## What Changes

- **BREAKING — `$` now means money, not a bare decimal.** `$` / `money` (aliases) build a
  **currency-bound** money value. `Decimal` is split off as the **exact-number engine** for
  everything that is *not* money (quantities, rates, weights, percentages) and is no longer an
  alias of `$`. Non-money uses of `$` migrate to `Decimal`.
- **A currency travels with every amount.** Currency resolves through a three-level cascade —
  explicit per-call (`$("19.99","JPY")`) → per-request `config.currency` (a script embeds its own
  currency) → operator-global `default_currency` → otherwise a plain-language error. The currency
  supplies the number of decimal places via a bundled **ISO 4217** minor-unit table (USD=2, JPY=0,
  BHD=3), so authors never name a decimal place.
- **Money is safe by construction.** Same-currency `add`/`sub` only (cross-currency → error),
  `mul`/`div` by a scalar only (money×money → error), and **no implicit FX** (the box holds no
  exchange rates). `pct` / `add_pct` / `sub_pct` express tax/discount/markup, auto-rounded to the
  currency.
- **Penny-safe splitting.** `allocate(weights)` / `allocate_to(n)` / `split(n)` distribute an
  amount so the parts sum to the whole **exactly**, using the largest-remainder (Hamilton) method
  with a deterministic, order-stable tie-break.
- **Standard, snake_case rounding.** Rounding modes adopt the industry meaning but the house
  dialect: `"half_up"` (default; backward-compatible + the retail/tax norm), `"half_even"`
  (banker's / accounting), plus directed modes as needed.
- **Interop-friendly serialization.** A money value **always** serializes as a self-describing
  `{ amount, currency, minor_units }` (integer `minor_units`, currency-correct across JPY/BHD),
  with `to_minor()` for building outbound integer-minor-unit payloads (Stripe/Square/Adyen),
  `format()` for a human `"$19.99"`, and `amount()` to drop down to a raw `Decimal`.
- **BREAKING — snake_case migration of the value-util surface.** Author-facing methods become
  snake_case everywhere (`to_cents`, `from_cents`, `is_zero`, `is_negative`, `to_number`), with the
  old camelCase spellings kept as **deprecated aliases** for one release. The JS-runtime protocol
  hooks the engine calls by fixed name (`toString`, `toJSON`, `valueOf`) keep their JS spelling —
  they are plumbing, not the business surface.

## Capabilities

### New Capabilities
- `money`: a currency-bound money value — construction + the three-level currency cascade,
  currency-safe arithmetic and comparison, business percentages (tax/discount/markup),
  largest-remainder allocation, currency-aware rounding to the minor unit, and the self-describing
  `{ amount, currency, minor_units }` serialization plus `to_minor` / `format` / `amount`.

### Modified Capabilities
- `decimal`: reframed from "the money-and-everything decimal" to the **exact-number engine** for
  non-money values. Money-specific behavior (minor-unit conversion) moves to `money`; the API
  migrates to snake_case (deprecated camelCase aliases retained); adds `round(places, mode)` with
  the standard rounding-mode vocabulary, `round_to(step, mode)`, and the bounded scalar helpers
  `clamp` / `min` / `max` / `pct`. Exactness, panic-free failure, and always-on injection are
  unchanged.

## Impact

- **Code (core):** `crates/runlet-core/src/decimal.rs` and `src/js/decimal.js` — split into a
  `money` wrapper + a `decimal` wrapper, add the largest-remainder allocator, the rounding-mode
  vocabulary (mapped onto `rust_decimal::RoundingStrategy`), and the static ISO 4217 exponent
  table. The FFI grows a path for a mode argument and for array (allocate) results.
- **Config:** server `Config` (`crates/runlet/src/config.rs`) gains `default_currency`; the
  per-request config gains `currency`. Both additive.
- **Types & docs:** `container/types.d.ts` (the D11 golden test `types_dts_is_up_to_date` must stay
  green), the bundled `tsconfig.json` `checkJs`, and the beginner docs (`docs/05-decimal.md` →
  money + decimal). The stale CLAUDE.md conventions bullet (value-utils use camelCase) is corrected.
- **Backward-compatibility:** two breaks — `$` changes meaning (decimal → money) and the value-util
  methods migrate to snake_case (aliased for one release). Both must be called out loudly in the
  changelog and migration docs.
- **Out of scope (deferred):** aggregate statistics (`sum` / `mean` / `weighted_mean` / `median` /
  `p95` / `stdev`) are batch-reduce-shaped and belong with the separate `batch-lifecycle-phases`
  change's `after` phase; genuine bulk analytics go to an external `io` service. `pow` / roots are
  omitted (rarely business; exactness is thorny).
