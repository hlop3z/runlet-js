# datetime Specification

## Purpose

The `datetime` capability is the always-on, pure date/time value-util for the sandbox: a
top-level factory that produces immutable UTC-canonical instants with a snake_case,
business-scripting author surface. It provides construction/parsing, calendar components,
calendar-aware arithmetic, difference, weekend-aware business-day helpers, comparison, period
boundaries, timezone-aware views, and numeric/ISO formatting — all pure (no I/O, no
per-operation metering) except the current-time reader `$std.datetime.now()`, which the
deterministic profile removes. It supersedes the former `$sys.date` helpers.

## Requirements

### Requirement: Always-on injection

The system SHALL inject a top-level `datetime` global into every execution context
unconditionally, because the capability is pure (no I/O, no per-operation metering) and needs no
capability config. `datetime` SHALL be a factory usable both as a callable (`$std.datetime(input)`) and
via named constructors (`$std.datetime.now()`, `$std.datetime.parse(input)`, `$std.datetime.from(parts, zone?)`).

#### Scenario: Available with no capability config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof datetime === "function"`, `$std.datetime.now`, `$std.datetime.parse`, and `$std.datetime.from` are callable

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs `datetime` operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Deterministic profile removes the clock

Under the deterministic execution profile the current-time reader `$std.datetime.now()` SHALL be
removed from the context — absent such that a script cannot re-reach it — not stubbed. All other
`datetime` behavior (parsing explicit inputs, components, arithmetic, comparison, formatting) SHALL
remain fully available, because it is pure given an explicit instant.

#### Scenario: now() is absent under the deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** `$std.datetime.now` is undefined and cannot be re-reached, while `$std.datetime.parse` / `$std.datetime.from` and all instance methods still work

#### Scenario: Same input is reproducible under the deterministic profile

- **WHEN** the same `(script, input)` parses an explicit instant and derives components/arithmetic from it, twice
- **THEN** both runs produce identical results

### Requirement: Construction and parsing normalize to a UTC instant

`$std.datetime.parse(input)` (and the callable `$std.datetime(input)`) SHALL accept an RFC 3339 / ISO 8601
string, a `YYYY-MM-DD` date-only string, epoch milliseconds, or an existing `datetime`, and return
an immutable value canonicalized to a UTC instant. `$std.datetime.from(parts, zone?)` SHALL build an
instant from `{year, month, day, hour?, minute?, second?, millisecond?}`, interpreting the parts in
`zone` when supplied else UTC. Unparseable or out-of-range input SHALL throw a developer/script error.

#### Scenario: Parse multiple input forms

- **WHEN** the handler calls `$std.datetime.parse` with an RFC 3339 string, a `YYYY-MM-DD` string, epoch millis, or another `datetime`
- **THEN** it returns a UTC-canonical instant, and unparseable input throws

#### Scenario: Construct from parts

- **WHEN** the handler calls `$std.datetime.from({ year: 2026, month: 7, day: 10 })`
- **THEN** it returns the corresponding UTC midnight instant, and supplying a `zone` interprets the parts in that zone

#### Scenario: Locale-format strings are not guessed

- **WHEN** the handler passes an ambiguous locale-formatted string such as `"07/10/2026"`
- **THEN** parsing throws rather than guessing a month/day order

### Requirement: Immutable value with ISO serialization

A `datetime` value SHALL be immutable — every operation returns a new value and never mutates the
receiver. Its canonical scalar SHALL be `epoch_ms()` (epoch milliseconds), and it SHALL serialize
to its RFC 3339 UTC string inside `json(...)` / `JSON.stringify` and when stringified.

#### Scenario: Operations do not mutate the receiver

- **WHEN** the handler calls `d.add({ days: 1 })`
- **THEN** a new value is returned and `d` is unchanged

#### Scenario: Serializes as RFC 3339 UTC

- **WHEN** a `datetime` value is returned via `json(...)` or stringified
- **THEN** it serializes to its RFC 3339 ISO string in UTC (`Z`), and `epoch_ms()` returns its epoch milliseconds

### Requirement: Snake_case author surface

Every `datetime` method name SHALL be `snake_case`, consistent with the sandbox's business-scripting
convention and the `money` / `Decimal` value-utils. In particular the epoch-milliseconds accessor
SHALL be `epoch_ms()`.

#### Scenario: Epoch accessor is snake_case

- **WHEN** the handler reads a value's epoch milliseconds
- **THEN** the method is `epoch_ms()` and no `epochMs` name exists

### Requirement: Calendar components

A `datetime` value SHALL expose its calendar components: `year()`, `month()` (1–12), `day()`
(1–31), `hour()`, `minute()`, `second()`, `millisecond()`, `weekday()` (ISO 1=Monday … 7=Sunday),
`quarter()` (1–4), `day_of_year()` (1–366), `iso_week()` (`{ week, week_year }`, ISO-8601), and
`days_in_month()`. Components SHALL resolve in UTC by default, or in the zone of a zoned view.

#### Scenario: Read components of an instant

- **WHEN** the handler reads components of `$std.datetime.parse("2026-07-10T13:30:00Z")`
- **THEN** `year()` is 2026, `month()` is 7, `day()` is 10, `weekday()` is 5 (Friday), `quarter()` is 3, and `days_in_month()` is 31

#### Scenario: ISO week reporting

- **WHEN** the handler calls `iso_week()` on a date in the first ISO week of a year
- **THEN** it returns `{ week, week_year }` following ISO-8601 week numbering (which may differ from the calendar year)

### Requirement: Calendar-aware arithmetic

`add(delta)` and `sub(delta)` SHALL accept a delta of `{ years?, months?, weeks?, days?, hours?,
minutes?, seconds?, ms? }` and return a shifted value. Month and year shifts SHALL clamp to a valid
day-of-month (e.g. Jan 31 + 1 month → the last day of February). Arithmetic that overflows the
representable range SHALL throw rather than wrap.

#### Scenario: Fixed-length units shift the instant

- **WHEN** the handler calls `.add({ days: 3, hours: 12 })` or `.sub({ weeks: 1 })`
- **THEN** the instant shifts by exactly that fixed duration

#### Scenario: Month arithmetic clamps end-of-month

- **WHEN** the handler calls `.add({ months: 1 })` on January 31
- **THEN** the result is the last valid day of February (28th or 29th), not an overflowed March date

#### Scenario: Overflow throws

- **WHEN** an arithmetic operation would exceed the representable instant range
- **THEN** the call throws a developer/script error

### Requirement: Difference between instants

`diff(other)` SHALL return the signed gap `this - other` as `{ total_ms, total_seconds, days,
hours, minutes, seconds }`, accepting another `datetime` or epoch millis. `diff_in(unit)` SHALL
return the signed whole-unit count for `unit` in `ms | seconds | minutes | hours | days | weeks`.

#### Scenario: Structured difference

- **WHEN** the handler calls `a.diff(b)`
- **THEN** it returns `{ total_ms, total_seconds, days, hours, minutes, seconds }` for `a - b`, signed

#### Scenario: Whole-unit difference

- **WHEN** the handler calls `a.diff_in("days")`
- **THEN** it returns the signed count of whole days between `a` and `b`

### Requirement: Weekend-aware business-day helpers

The value SHALL provide `is_weekend()`, `is_business_day()`, and `add_business_days(n)` treating
Saturday and Sunday as non-business days. These helpers SHALL NOT account for holidays, which are
country/company-specific and out of scope.

#### Scenario: Weekend detection

- **WHEN** the handler calls `is_weekend()` on a Saturday or Sunday instant
- **THEN** it returns `true`, and `is_business_day()` returns `false`

#### Scenario: Business-day addition skips weekends

- **WHEN** the handler calls `.add_business_days(1)` on a Friday
- **THEN** the result is the following Monday

### Requirement: Comparison without reading the clock

The value SHALL provide `cmp(other)` (`-1`/`0`/`1`), `eq`, `lt`, `lte`, `gt`, `gte`, and
`is_between(a, b)`, comparing by instant. No comparison helper SHALL read the ambient wall clock;
comparing against "now" requires an explicit `$std.datetime.now()` argument.

#### Scenario: Ordering comparisons

- **WHEN** the handler calls `a.lt(b)` / `a.eq(b)` / `a.is_between(lo, hi)`
- **THEN** the result reflects the instant ordering, and no method implicitly reads the current time

### Requirement: Period boundaries

`start_of(unit)` and `end_of(unit)` SHALL return the boundary instant for `unit` in `day | week |
month | quarter | year`. Weeks SHALL start on Monday (ISO) by default. When invoked on a zoned view
the boundary SHALL be computed in that view's zone.

#### Scenario: Month boundaries

- **WHEN** the handler calls `.start_of("month")` and `.end_of("month")` on a mid-month instant
- **THEN** it returns the first-instant and last-instant of that month respectively

#### Scenario: Quarter and ISO-week boundaries

- **WHEN** the handler calls `.start_of("quarter")` or `.start_of("week")`
- **THEN** it returns the first instant of the calendar quarter, or of the ISO week (Monday) respectively

### Requirement: Timezone-aware views

`in_zone(zone)` SHALL accept an IANA timezone name (e.g. `"America/New_York"`) and return a zoned
*view* whose components, period boundaries, and formatting resolve in that zone, while the
underlying canonical value remains the same UTC instant. An unknown zone name SHALL throw.

#### Scenario: Boundaries computed in a target zone

- **WHEN** the handler calls `d.in_zone("Asia/Tokyo").end_of("month").iso()`
- **THEN** the month boundary is computed in Tokyo local time and rendered as the corresponding instant

#### Scenario: Canonical instant is preserved

- **WHEN** the handler calls `d.in_zone("Asia/Tokyo")` and reads `epoch_ms()`
- **THEN** the epoch milliseconds equal those of the original UTC `d` (only the interpretation zone changed)

#### Scenario: Unknown zone rejected

- **WHEN** the handler calls `in_zone` with an unrecognized timezone name
- **THEN** the call throws a developer/script error

### Requirement: Numeric and ISO formatting

The value SHALL provide `iso(zone?)` (RFC 3339, UTC `Z` by default or the given zone's offset),
`unix()` (epoch seconds), `epoch_ms()` (epoch milliseconds), and `format(pattern, zone?)` using
locale-neutral numeric field tokens. Formatting SHALL NOT depend on locale-language month/day names.

#### Scenario: ISO and epoch renderings

- **WHEN** the handler calls `iso()`, `unix()`, and `epoch_ms()`
- **THEN** it returns the RFC 3339 string, epoch seconds, and epoch milliseconds of the same instant

#### Scenario: Numeric pattern formatting

- **WHEN** the handler calls `format("YYYY-MM-DD HH:mm", "America/New_York")`
- **THEN** it renders the numeric fields in that zone with no locale-language names required
