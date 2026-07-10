## ADDED Requirements

### Requirement: Always-on injection

The system SHALL inject the `$` / `money` global into every execution context unconditionally,
because the capability is pure (no I/O, no per-op metering) and needs no capability config. `$`
and `money` SHALL be the same constructor.

#### Scenario: Available with no capability config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof $ === "function"` and `typeof money === "function"`, and `$` and `money` are the same constructor

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs money operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Money construction and the currency cascade

`$(amount, currency?)` / `money(amount, currency?)` SHALL build a currency-bound money value from
an `amount` (string, number, `Decimal`, or another money) and a currency resolved through a
three-level cascade: (1) an explicit `currency` argument, else (2) the per-request
`config.currency`, else (3) the operator-global `default_currency`. If none of the three yields a
currency, construction SHALL throw a plain-language error.

#### Scenario: Explicit currency argument wins

- **WHEN** a handler calls `$("1000", "JPY")`
- **THEN** the value's currency is `JPY` regardless of `config.currency` or the operator default

#### Scenario: Per-request config supplies the currency

- **WHEN** the request sets `config.currency = "EUR"` and a handler calls `$("19.99")`
- **THEN** the value's currency is `EUR`

#### Scenario: Operator default supplies the currency

- **WHEN** no `currency` argument is given and the request has no `config.currency`, but the operator configured `default_currency = "USD"`
- **THEN** `$("19.99")` produces a value whose currency is `USD`

#### Scenario: No currency resolvable throws

- **WHEN** a handler calls `$("19.99")` with no argument, no `config.currency`, and no operator `default_currency`
- **THEN** a catchable JavaScript error is thrown explaining that a currency must be set (e.g. `$(19.99, "USD")`)

#### Scenario: Unknown currency code throws

- **WHEN** a handler calls `$("10", "ZZZ")` with a code absent from the ISO 4217 table
- **THEN** a catchable JavaScript error is thrown naming the unrecognized currency

### Requirement: Currency-defined precision (ISO 4217)

The number of decimal places for a money value SHALL be determined by its currency's ISO 4217
minor-unit exponent from a bundled static table, not supplied by the author. A currency with
exponent 0 SHALL have no fractional part; a currency with exponent 3 SHALL have three.

#### Scenario: Two-decimal currency

- **WHEN** a handler rounds `$("19.985", "USD")` to its currency precision
- **THEN** the result is `19.99` (USD exponent 2)

#### Scenario: Zero-decimal currency

- **WHEN** a handler rounds `$("1000.4", "JPY")` to its currency precision
- **THEN** the result is `1000` (JPY exponent 0)

#### Scenario: Three-decimal currency

- **WHEN** a handler rounds `$("1.2345", "BHD")` to its currency precision
- **THEN** the result is `1.234` (BHD exponent 3)

### Requirement: Currency-safe arithmetic

Money arithmetic SHALL be safe by construction: `add` and `sub` SHALL operate only on two money
values of the **same** currency; `mul` SHALL accept only a scalar (number, string, or `Decimal`) and
SHALL reject a money operand; `div` SHALL accept **either** a scalar — returning a money value — **or**
another money value of the **same** currency — returning a dimensionless `Decimal` ratio (as ERPs do
for margin/variance/growth). Adding or subtracting across currencies, multiplying money by money, and
dividing by a different-currency money SHALL throw a catchable plain-language error. The system SHALL
NOT perform any implicit currency conversion (it holds no exchange rates).

#### Scenario: Same-currency addition

- **WHEN** a handler evaluates `$("19.99","USD").add($("5.00","USD"))`
- **THEN** the result is `24.99 USD`

#### Scenario: Cross-currency addition throws

- **WHEN** a handler evaluates `$("19.99","USD").add($("5.00","EUR"))`
- **THEN** a catchable error is thrown reporting the currency mismatch, and no implicit conversion occurs

#### Scenario: Scalar multiplication

- **WHEN** a handler evaluates `$("19.99","USD").mul(3)`
- **THEN** the result is `59.97 USD`

#### Scenario: Money-times-money throws

- **WHEN** a handler evaluates `$("2.00","USD").mul($("3.00","USD"))`
- **THEN** a catchable error is thrown (money multiplied by money is not a money)

#### Scenario: Same-currency money-over-money yields a ratio

- **WHEN** a handler evaluates `$("115.00","USD").div($("100.00","USD"))`
- **THEN** the result is a `Decimal` of `1.15` — a dimensionless ratio, not a money

#### Scenario: Cross-currency division throws

- **WHEN** a handler evaluates `$("115.00","USD").div($("100.00","EUR"))`
- **THEN** a catchable currency-mismatch error is thrown (no implicit conversion)

### Requirement: Business percentages

The system SHALL provide `pct(p)`, `add_pct(p)`, and `sub_pct(p)` for expressing tax, discount,
and markup: `pct(p)` returns `p` percent of the value; `add_pct(p)` returns the value increased by
`p` percent; `sub_pct(p)` returns the value decreased by `p` percent. Each result SHALL be a money
value in the same currency, rounded to the currency's precision.

#### Scenario: Percentage of an amount

- **WHEN** a handler evaluates `$("200.00","USD").pct(8.25)`
- **THEN** the result is `16.50 USD`

#### Scenario: Add a percentage (tax/markup)

- **WHEN** a handler evaluates `$("100.00","USD").add_pct(8.25)`
- **THEN** the result is `108.25 USD`

#### Scenario: Subtract a percentage (discount)

- **WHEN** a handler evaluates `$("50.00","USD").sub_pct(10)`
- **THEN** the result is `45.00 USD`

### Requirement: Penny-safe allocation

The system SHALL provide `allocate(weights)` (weighted split), `allocate_to(n)` (equal split into
`n` shares), and `split(n)` (an alias of `allocate_to`). Each SHALL return an array of money values
in the source currency whose sum equals the original amount **exactly**, using the largest-remainder
(Hamilton) method: each share floored to the currency's minor unit, then the leftover minor units
distributed one at a time to the shares with the largest fractional remainders. Tie-breaking SHALL
be deterministic and order-stable (earlier-listed shares receive leftover units first), so identical
input always yields identical output. The number of returned shares SHALL equal `n` (or the number
of weights), including zero-weight shares.

#### Scenario: Equal split preserves the total

- **WHEN** a handler evaluates `$("100.00","USD").allocate_to(3)`
- **THEN** the result is `[33.34, 33.33, 33.33]` and the three shares sum to exactly `100.00 USD`

#### Scenario: Weighted split preserves the total

- **WHEN** a handler evaluates `$("0.05","USD").allocate([70, 30])`
- **THEN** the result is `[0.04, 0.01]` (the leftover cent goes to the larger-remainder, earlier share) and the shares sum to exactly `0.05 USD`

#### Scenario: Zero-decimal currency allocates in whole units

- **WHEN** a handler evaluates `$("1000","JPY").allocate_to(3)`
- **THEN** the result is `[334, 333, 333]` summing to exactly `1000 JPY`

#### Scenario: Deterministic tie-break

- **WHEN** the same allocation input is evaluated twice
- **THEN** both evaluations return the identical distribution of leftover units

#### Scenario: split is an alias of allocate_to

- **WHEN** a handler calls `.split(n)`
- **THEN** it behaves identically to `.allocate_to(n)`

### Requirement: Currency-aware rounding

`round(mode?)` SHALL round a money value to its currency's minor unit, where `mode` selects a
rounding strategy from the standard vocabulary and defaults to `"half_up"`. The author SHALL NOT
need to supply a decimal-place count — the currency provides it.

#### Scenario: Default rounds to the currency minor unit

- **WHEN** a handler evaluates `$("1.005","USD").round()`
- **THEN** the result is `1.01 USD` (half_up to 2 places)

#### Scenario: Banker's rounding is selectable

- **WHEN** a handler evaluates `$("1.005","USD").round("half_even")`
- **THEN** the result is `1.00 USD`

### Requirement: Currency-safe comparison

The system SHALL provide `cmp`, `eq`, `lt`, `lte`, `gt`, `gte`, `is_zero`, `is_negative`, and
`is_positive` over money values. The binary comparisons SHALL require the same currency on both
sides and SHALL throw a catchable error on a currency mismatch.

#### Scenario: Same-currency ordering

- **WHEN** a handler evaluates `$("19.99","USD").gt($("9.99","USD"))`
- **THEN** the result is `true`

#### Scenario: Cross-currency comparison throws

- **WHEN** a handler evaluates `$("19.99","USD").gt($("9.99","EUR"))`
- **THEN** a catchable currency-mismatch error is thrown

#### Scenario: Sign predicates

- **WHEN** a handler evaluates `$("-1.00","USD").is_negative()`
- **THEN** the result is `true`

### Requirement: Self-describing serialization

A money value SHALL always serialize inside `json(...)` / `JSON.stringify` as a self-describing
object `{ amount, currency, minor_units }`: `amount` the exact decimal string, `currency` the ISO
4217 code, and `minor_units` the integer count of minor units. `minor_units` SHALL be
currency-correct (derived from the ISO 4217 exponent, not assumed to be hundredths).

#### Scenario: Money serializes with all three fields

- **WHEN** a handler returns `json({ total: $("19.99","USD") }, null)`
- **THEN** the response `data.total` is `{ "amount": "19.99", "currency": "USD", "minor_units": 1999 }`

#### Scenario: minor_units follows the currency exponent

- **WHEN** a handler serializes `$("1000","JPY")`
- **THEN** `minor_units` is `1000` (JPY exponent 0), not `100000`

### Requirement: Read-out and interop helpers

A money value SHALL expose `to_minor()` (integer minor units for building outbound payment
payloads), `amount()` (the value as an exact `Decimal`, currency stripped), `currency()` (the ISO
code), `format()` (a human display string such as `"$19.99"`), `to_string()` (the exact amount
string), and `to_number()` (a lossy JS number). `to_minor()` SHALL be zero-decimal-correct via the
currency exponent.

#### Scenario: to_minor for a payment API

- **WHEN** a handler evaluates `$("19.99","USD").to_minor()`
- **THEN** the result is the integer `1999`, suitable as an integer-minor-unit amount for a payment API

#### Scenario: to_minor is zero-decimal-correct

- **WHEN** a handler evaluates `$("1000","JPY").to_minor()`
- **THEN** the result is `1000`, not `100000`

#### Scenario: amount drops down to Decimal

- **WHEN** a handler evaluates `$("19.99","USD").amount()`
- **THEN** the result is a `Decimal` whose `to_string()` is `"19.99"` and which carries no currency

### Requirement: Panic-free failure

Every money operation SHALL use checked arithmetic and surface overflow, division by zero, parse
failures, currency mismatches, and unknown currencies as catchable JavaScript errors rather than
crashing the engine.

#### Scenario: Division by zero throws

- **WHEN** a handler evaluates `$("10","USD").div(0)`
- **THEN** a catchable JavaScript error is thrown (no panic, no process abort)

#### Scenario: Overflow throws

- **WHEN** an operation produces a value outside the representable decimal range
- **THEN** a catchable `"overflow"` error is thrown
