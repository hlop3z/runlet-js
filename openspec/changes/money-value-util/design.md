## Context

The `$` / `Decimal` global (`crates/runlet-core/src/decimal.rs` + `src/js/decimal.js`) is today a
single exact-decimal value-util backed by `rust_decimal`, injected into every context. It is
currency-blind: authors track decimal places, rounding modes, and penny-safe splitting by hand.
That mismatches the product's identity — a **business-made scripting language** with a Python-like
feel for non-developers (snake_case surface; see the memory note `business-scripting-snake-case`) —
and it puts the most error-prone, compliance-sensitive parts of money math (VAT rounding, tax/refund
splits, per-currency minor units) on the author.

This change reframes the surface into two globals — a currency-safe `money` (`$`) and an
exact-number `Decimal` — and adopts **established international/industry standards** rather than
invented conventions, exposed behind a dead-simple API. The correctness core is small: a rounding-mode
vocabulary, a static ISO 4217 exponent table, and a largest-remainder allocator. Everything else is
composition.

Constraints: the strict lint gauntlet (no `unwrap`/`expect`/`panic`, no bare arithmetic — use
`checked_*`, no `as` casts); the FFI boundary is the stringly-typed `__decimal(op, lhs, rhs)`; the
JS wrapper is eval'd per `Context`; build/test are Docker-only (WDAC blocks native cargo — memory
`wdac-blocks-native-cargo`); the D11 golden test `types_dts_is_up_to_date` gates `container/types.d.ts`.

## Goals / Non-Goals

**Goals:**
- A currency-bound `money` value that is correct-by-default and reads like plain business language.
- Adopt standard vocabularies (ISO 4217 exponents, the Java-derived rounding-mode names, the
  Fowler/largest-remainder allocation pattern) — spelled in snake_case dialect.
- Keep the correctness core minimal; compose the rest in the JS wrapper.
- Preserve exactness and panic-free failure throughout.

**Non-Goals:**
- **No foreign-exchange / currency conversion.** The box holds no exchange rates (they are stale-able
  operational data, unlike the stable ISO 4217 exponents). Cross-currency math throws.
- **No aggregate statistics** (`sum`/`mean`/`weighted_mean`/`median`/`p95`/`stdev`) — batch-reduce
  shaped; deferred to the `batch-lifecycle-phases` `after` phase, with bulk analytics pushed to `io`.
- **No `pow`/roots** — rarely business, exactness is thorny.
- No new external crate: `rust_decimal` already implements every rounding strategy we need.

## Decisions

### Decision: `$` becomes money; `Decimal` becomes the exact-number engine
The `$` glyph *is* a money sign, so it carries the headline abstraction for free. `$`/`money` are
aliases (a currency-bound value); `Decimal` is split off for non-money numbers and is no longer a
`$` alias. This is a **breaking change** but it is the whole point — it lets money be safe by
construction and lets a non-developer never think about decimal places.
- **Alternatives considered:** (a) keep `$` = decimal and add a separate `money(...)` global — but
  then the money-sign glyph is wasted on non-money and two constructors compete for the headline;
  (b) a single value that is "sometimes money" — reintroduces the ambiguity we are trying to remove.

### Decision: Currency granularity from a bundled static ISO 4217 exponent table
"Name the currency, get its decimal places" is the universal idiom — every major money library
(JavaMoney, Joda-Money, moneyphp, dinero.js) derives places from the currency, and `Intl.NumberFormat`
makes it the default. ISO 4217 (maintained by SIX on behalf of ISO) carries a minor-unit *exponent*
per currency; the exponents are **stable and event-driven** (they change only on redenomination /
new-currency events, not on a cadence), so embedding a static table is safe, standard practice.
- Refs: ISO 4217 (SIX-maintained list); `Intl.NumberFormat` currency style (MDN).
- **This reverses an earlier caution** ("don't embed a currency table — it goes stale"): that risk
  is real for *exchange rates*, which we still never touch, but not for exponents.
- **Placement:** the table is pure data. It can live as a JS lookup object in the wrapper
  (currency → exponent, then call the existing numeric ops) so **currency-awareness costs zero Rust**,
  or as a Rust `match`. Leaning JS-side to keep currency *policy* as data and the Rust FFI minimal.

### Decision: Rounding modes adopt the Java `RoundingMode` meaning, spelled snake_case
The industry lingua franca is Java's `RoundingMode` (`HALF_UP`, `HALF_EVEN`, …), which Python
`decimal`, .NET `MidpointRounding`, dinero.js, and IEEE 754 all map onto. We adopt the **meaning**
and render the **identifiers** in the house dialect: `"half_up"`, `"half_even"`, `"up"`, `"down"`,
`"ceil"`, `"floor"`. `rust_decimal::RoundingStrategy` already implements each one, so this is a
name-mapping, not new math.
- Default stays **`half_up`** (half-away-from-zero): backward-compatible with today's `round`, and
  the researched retail/invoice/tax default. `half_even` (banker's) is the opt-in for ledgers.
- Refs: Java `RoundingMode`; IEEE 754 rounding-direction attributes; Python `decimal` `ROUND_*`.

### Decision: Allocation via the largest-remainder (Hamilton) method
`allocate(weights)` / `allocate_to(n)` / `split(n)` split an amount so the parts sum to the whole
**exactly**: floor each share to the currency minor unit, then distribute leftover minor units to the
largest fractional remainders, breaking ties by input order (deterministic — a hard sandbox
invariant). This is Martin Fowler's `Money.allocate` pattern; the algorithm is the Largest Remainder
Method (a.k.a. Hamilton's method) from apportionment theory, and it is what moneyphp, dinero.js,
Ruby Money, and go-money all implement.
- Refs: Fowler, *PoEAA* — Money pattern; Largest Remainder / Hamilton's method.
- **Alternative rejected:** "make the last share the remainder" — piles all rounding drift onto one
  share; the largest-remainder approach spreads it and is the industry norm.

### Decision: Three-level currency cascade
Resolve currency as explicit arg → per-request `config.currency` → operator `default_currency` →
else a plain-language error. Mirrors the box's existing config layering (engine/server config), lets
a script embed its own currency once (`config.currency`) instead of repeating it per call, and lets
an operator set a box default. Additive config fields only.

### Decision: Money always serializes as `{ amount, currency, minor_units }`
At the response boundary a consumer (frontend, billing service) shouldn't have to re-derive cents, so
`to_json` is always self-describing. The integer field is **`minor_units`** (currency-correct across
JPY=0 / BHD=3), not `cents`. It is a JSON integer (payment APIs demand an integer; safe up to
~$90T in cents — a documented ceiling; the one deliberate spot we trade a sliver of theoretical
exactness for interop). Building an *outbound* payment payload (Stripe/Square/Adyen, which want the
provider's own field names) is served by the `to_minor()` method, which is zero-decimal-correct via
the exponent. `format()` gives the human `"$19.99"`; `amount()` drops to a raw `Decimal`.

### Decision: snake_case everywhere except JS protocol hooks
All author-facing methods are snake_case (`to_cents`→`to_minor` on money, `is_zero`, `to_number`),
with old camelCase spellings kept as deprecated aliases for one release. The engine-invoked hooks
`toString` / `toJSON` / `valueOf` keep their JS spelling — QuickJS calls them by those exact names,
so they cannot be renamed; they are invisible plumbing, not the business surface.

### Decision: FFI evolution — minimal, two shapes
The current FFI is `__decimal(op, lhs, rhs) → {v} | {error}` (scalar in, one scalar out). This change
needs two extensions: (1) carry a rounding **mode** alongside `places` (pack into `rhs`, or widen the
call), and (2) return an **array** for `allocate` (a `{list:[...]}` envelope path). The bounded scalar
helpers (`clamp`/`min`/`max`/`pct`) need **no** Rust — they compose in JS over existing `cmp`/`mul`.
Prefer the smallest change that keeps the wrapper readable; do not build a general vec-FFI for the
deferred `stats` quadrant (YAGNI).

## Risks / Trade-offs

- **[Breaking: `$` changes meaning decimal → money]** → Loud changelog + migration doc: non-money
  uses of `$` move to `Decimal`; provide the mapping. Consider a transition-period lint/doc note.
- **[Breaking: camelCase → snake_case]** → Keep camelCase spellings as deprecated aliases for one
  release so existing scripts don't break immediately.
- **[`minor_units` as a JSON integer can overflow beyond ~$90T]** → Documented ceiling; realistic
  commerce is far below `2^53` minor units. Exact paths (`amount()`, `to_string()`) remain string-based.
- **[ISO 4217 table drifts when a currency redenominates]** → Rare, event-driven; refresh the static
  table on ISO amendment (same maintenance every money library accepts). No rates, so no daily drift.
- **[Tax-rounding is jurisdiction-specific]** → We provide the *mechanism* (per-line rounding,
  selectable mode) not a *policy*. Default per-line `half_up` to the minor unit follows the neutral
  CJEU *Ahold* (C-484/06) / HMRC VAT Notice 700 §17.5 consensus; the author composes the jurisdiction
  rule. We do not hard-code rounding-down (a UK concession, not a general rule).
- **[JS wrapper grows a ~180-entry currency table, eval'd per Context]** → A few KB parsed once per
  request; negligible against runtime warm-up. If it ever matters, move the table to a Rust `match`.

## Migration Plan

1. Ship `Decimal` (exact-number engine) and `$`/`money` (currency-bound) side by side; keep camelCase
   method aliases on `Decimal`.
2. Update `container/types.d.ts` (D11 golden test), `tsconfig.json` `checkJs`, and beginner docs
   (`docs/05-decimal.md` → money + decimal). Correct the stale CLAUDE.md conventions bullet.
3. Publish the migration mapping (`$`-as-decimal → `Decimal`; `toCents` → `to_minor`).
4. Remove the deprecated camelCase aliases one release later.

Rollback: the change is additive at the config layer and self-contained to the value-util; reverting
the wrapper + `decimal.rs` restores prior behavior. No data migration.

## Resolved (ERP-grounded)

The four questions carried out of exploration were settled by surveying six ERPs — SAP S/4HANA,
Oracle NetSuite, Microsoft Dynamics 365, Odoo, ERPNext/Frappe, and QuickBooks/Xero.

1. **`money.to_string()` → amount-only (`"19.99"`).** Unanimous across all six: the amount is stored
   and rendered as a bare number with the currency in a *separate* field (SAP `CURR`+`CUKY`, NetSuite
   currency-on-transaction, Dynamics `AmountCur`+`CurrencyCode`, Odoo `currency_id`, QBO `CurrencyRef`,
   Xero `CurrencyCode`); combining is always an explicit format step. `format()` owns the symbol form.
   Refs: SAP CURR/CUKY (help.sap.com abap currency field); NetSuite `N/format/i18n getCurrencyFormatter`;
   Odoo `Monetary`→`res.currency`; QBO `CurrencyRef`; Xero `CurrencyCode`.
2. **`div` is overloaded: `div(scalar) → money`, `div(same-currency money) → Decimal ratio`.** The
   universal ERP pattern is money ÷ scalar → money (unit price) and money ÷ money → a dimensionless
   ratio/percentage (margin, variance, growth) — e.g. ERPNext margin `(sell−buy)/sell`, ABAP
   `DIVISION()` for percentages. Cross-currency `div` and money×money throw. This **reverses** the
   explore-phase "scalar-only" lean. Refs: ERPNext gross-margin; ABAP numeric ops; Odoo/QBO scalar
   pricing.
3. **Default `half_up`; ship the fuller mode set from the start.** Out-of-the-box default is half-up
   (commercial rounding) in SAP, NetSuite, Dynamics, Odoo, QBO — while banker's (half-even) is a
   *shipped* default in Frappe (`rounded()` legacy) and Xero, so half-even is a required first-class
   option, not a nicety. Directed rounding is commonly offered (Dynamics Normal/Up/Down; Odoo
   `UP`/`DOWN`/`HALF-DOWN`), and SAP's per-currency rounding **unit** (CHF 0.05) is exactly our
   `round_to(step)`. So ship `half_up` (default), `half_even`, `up`, `down`, `ceil`, `floor` — all
   free via `rust_decimal`. Refs: SAP `round()`/`T001R`; Odoo `float_round`; Frappe `rounded()`;
   Xero rounding guide; Dynamics `ROUNDAMOUNT`.
4. **Currency table lives in the JS wrapper; Rust receives a resolved `places` integer.** Even the
   Rust-side ops (`allocate`/`round`/`to_minor`) never need the table: JS resolves currency → exponent
   and passes `places` across the FFI. Currency granularity is not a security boundary (unlike the
   SSRF allowlists, which must stay Rust-side), so a script-visible JS table is safe; a tampered table
   only affects that request's own arithmetic. Keeps currency-awareness zero-Rust.
