## MODIFIED Requirements

### Requirement: Always-on injection

The system SHALL inject the `Decimal` global into every execution context unconditionally, because
the capability is pure (no I/O, no per-op metering) and takes no configuration. `Decimal` SHALL NOT
be an alias of `$` — `$` now constructs a currency-bound money value (see the `money` capability),
while `Decimal` is the exact-number engine for non-money values (quantities, rates, weights,
percentages).

#### Scenario: Available with no config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof Decimal === "function"`

#### Scenario: Decimal is distinct from money

- **WHEN** a handler inspects the globals
- **THEN** `Decimal !== $` and `$` constructs money while `Decimal` constructs an exact number

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs decimal operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Decimal construction

`Decimal(value)` SHALL build a decimal from a string, a number, or another decimal, preserving
exact value when the input is a string.

#### Scenario: Construct from a string

- **WHEN** a handler calls `Decimal("2.5")`
- **THEN** a decimal whose `to_string()` is `"2.5"` is produced

#### Scenario: Construct from an existing decimal

- **WHEN** a handler passes a decimal back into `Decimal(...)`
- **THEN** the same value is returned without re-parsing

#### Scenario: Invalid input throws

- **WHEN** a handler calls `Decimal("not-a-number")`
- **THEN** a JavaScript error is thrown that the handler can catch with `try/catch`

### Requirement: Method-based arithmetic

The system SHALL expose arithmetic as chainable methods (`add`, `sub`, `mul`, `div`, `neg`, `abs`)
— not the `+ - * /` operators — each returning a new decimal and coercing a number, string, or
decimal argument.

#### Scenario: Chained arithmetic

- **WHEN** a handler evaluates `Decimal("19.99").mul(3).add("0.01").to_string()`
- **THEN** the result is the exact string `"59.98"`

#### Scenario: Method arguments are coerced

- **WHEN** a method receives a number, string, or another decimal as its argument
- **THEN** it is coerced to a decimal before the operation

### Requirement: Exactness

Decimal arithmetic SHALL be exact in base 10, free of the binary-floating-point drift that afflicts
native JS number math.

#### Scenario: No 0.1 + 0.2 drift

- **WHEN** a handler evaluates `Decimal("0.1").add("0.2").to_string()`
- **THEN** the result is exactly `"0.3"`, not `0.30000000000000004`

### Requirement: Half-up rounding

`round(places, mode)` SHALL round to `places` decimal places (default `0`) using the rounding
strategy named by `mode`, which SHALL default to `"half_up"` (half-away-from-zero) for
backward-compatibility. The system SHALL also provide `round_to(step, mode)` to round to the nearest
multiple of `step` (e.g. `"0.05"` for cash rounding).

#### Scenario: Round to places (default mode)

- **WHEN** a handler evaluates `Decimal("19.985").round(2).to_string()`
- **THEN** the result is `"19.99"`

#### Scenario: Default places

- **WHEN** a handler calls `.round()` with no argument
- **THEN** it rounds to 0 decimal places using `half_up`

#### Scenario: Round to a step

- **WHEN** a handler evaluates `Decimal("2.03").round_to("0.05").to_string()`
- **THEN** the result is `"2.05"`

### Requirement: Comparison

The system SHALL provide comparison helpers (`cmp`, `eq`, `lt`, `lte`, `gt`, `gte`, `is_zero`,
`is_negative`, `is_positive`) over exact decimal values.

#### Scenario: Ordering predicates

- **WHEN** a handler evaluates `Decimal("19.99").gt("9.99")`
- **THEN** the result is `true`

#### Scenario: cmp tri-state

- **WHEN** a handler calls `.cmp(x)`
- **THEN** it returns `-1`, `0`, or `1` for less-than, equal, or greater-than

#### Scenario: Sign predicates

- **WHEN** a handler evaluates `Decimal("0").is_zero()`
- **THEN** the result is `true`

### Requirement: Panic-free failure

Every decimal operation SHALL use checked arithmetic and surface overflow, division by zero, and
parse failures as catchable JavaScript errors rather than crashing the engine.

#### Scenario: Division by zero throws

- **WHEN** a handler evaluates `Decimal("10").div(0)`
- **THEN** a JavaScript error is thrown (no panic, no process abort)

#### Scenario: Overflow throws

- **WHEN** an operation produces a value outside the decimal's representable range
- **THEN** a `"decimal overflow"` error is thrown that the handler can catch

### Requirement: Output and serialization

A decimal SHALL expose `to_string()` for its exact text and `to_number()` for a lossy JS number, and
SHALL serialize to its exact string in `json(...)` / `JSON.stringify`.

#### Scenario: Exact string output

- **WHEN** a handler calls `.to_string()` on a decimal
- **THEN** it returns the exact decimal text (e.g. `"59.98"`)

#### Scenario: Auto-stringified in the response

- **WHEN** a handler returns a decimal inside `json(data, error)`
- **THEN** the decimal is serialized as its exact string value in the response `data`

## ADDED Requirements

### Requirement: Standard rounding-mode vocabulary

`round` / `round_to` (on `Decimal`) and `round` (on `money`) SHALL accept a `mode` drawn from a
fixed vocabulary that adopts the established industry meaning rendered in the box's snake_case
dialect: `"half_up"` (half-away-from-zero, the default), `"half_even"` (banker's rounding), and the
directed modes `"up"`, `"down"`, `"ceil"`, `"floor"` (ERPs commonly offer up/down/nearest, and these
are free via `rust_decimal`). Each SHALL map onto the corresponding exact rounding strategy. An
unrecognized mode SHALL throw a catchable error.

#### Scenario: half_even is banker's rounding

- **WHEN** a handler evaluates `Decimal("2.5").round(0, "half_even").to_string()` and `Decimal("3.5").round(0, "half_even").to_string()`
- **THEN** the results are `"2"` and `"4"` respectively (ties go to the even neighbor)

#### Scenario: Unknown mode throws

- **WHEN** a handler passes a `mode` not in the vocabulary (e.g. `"sideways"`)
- **THEN** a catchable error is thrown naming the invalid mode

### Requirement: Bounded scalar helpers

`Decimal` SHALL provide `clamp(lo, hi)`, `min(other)`, `max(other)`, and `pct(p)`. `clamp` returns
the value constrained to the inclusive `[lo, hi]` range; `min`/`max` return the smaller/larger of
the value and the argument; `pct(p)` returns `p` percent of the value.

#### Scenario: Clamp to a range

- **WHEN** a handler evaluates `Decimal("120").clamp(0, 100).to_string()`
- **THEN** the result is `"100"`

#### Scenario: Percentage of a value

- **WHEN** a handler evaluates `Decimal("200").pct(15).to_string()`
- **THEN** the result is `"30"`

### Requirement: snake_case naming with deprecated aliases

Every author-facing method on `Decimal` SHALL be named in snake_case. The methods that were
previously camelCase (`isZero`, `isNegative`, `toNumber`) SHALL be renamed to their snake_case forms
(`is_zero`, `is_negative`, `to_number`) and the old camelCase spellings SHALL remain available as
**deprecated aliases** for one release. The JS-runtime protocol hooks the engine invokes by fixed
name (`toString`, `toJSON`, `valueOf`) SHALL keep their JS spelling.

#### Scenario: snake_case is canonical

- **WHEN** a handler calls `Decimal("5").is_zero()` and `Decimal("5").to_number()`
- **THEN** both resolve to the snake_case methods

#### Scenario: camelCase alias still works (deprecated)

- **WHEN** a handler calls the legacy `Decimal("5").isZero()`
- **THEN** it returns the same result as `is_zero()` (retained as a deprecated alias)

#### Scenario: Protocol hooks keep JS spelling

- **WHEN** the engine serializes a decimal via `JSON.stringify`
- **THEN** it invokes `toJSON` (JS spelling), which returns the exact string value

## REMOVED Requirements

### Requirement: Major/minor unit conversion

**Reason**: Minor-unit conversion is inherently currency-defined (the exponent depends on the
currency), so it belongs on the currency-bound `money` value, not on the currency-less `Decimal`.
The behavior moves to `money.to_minor()` (major → integer minor units) and money construction
(minor → major), which derive the scale from the ISO 4217 exponent instead of a caller-supplied
`places`.

**Migration**: Replace `$("19.99").toCents()` with `$("19.99","USD").to_minor()`; the currency now
supplies the number of minor-unit digits, so the `places` argument is no longer passed. For a bare
exact number that is genuinely not money, scale explicitly with `Decimal(...).mul(100).round(0)`.
