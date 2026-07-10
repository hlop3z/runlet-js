# list Specification

## Purpose

The `list` capability is the always-on, pure collection value-util for the sandbox: a top-level
factory that wraps an array of records as an immutable value with a snake_case, **field-name-first,
callback-free** author surface. It provides SQL / Shopify-Liquid-named shaping verbs
(`where`/`sort_by`/`group_by`/`column`/`unique`) and exact-`Decimal` aggregates (`sum`/`avg`/`min`/
`max`/`count`), plus positional access and interop. It is pure (no I/O, no clock, no randomness, no
per-operation metering) and is injected identically under every profile. `group_by` returns a
`dict`, making `list` and `dict` one matched design.

## Requirements

### Requirement: Always-on injection under every profile

The system SHALL inject a top-level `list` global into every execution context unconditionally and
identically under both `Profile::Full` and `Profile::Deterministic`, because the capability is pure
— it reads no clock, no randomness, and no ambient state — and needs no capability config. `list`
SHALL be a factory usable as a callable (`list(input)`) that coerces its argument to an array and
returns an immutable `list` value. Because the surface is pure, it SHALL NOT expose any random-order
operation (no `shuffle`/`sample`).

#### Scenario: Available with no capability config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof list === "function"` and `list([1,2,3])` returns a value with the list methods

#### Scenario: Present and identical under the deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** `list` and all of its methods are available and behave exactly as under the full profile (nothing is removed or stubbed)

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs `list` operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Immutable value with unwrap, iteration, and positional access

A `list` value SHALL be immutable: every method that transforms content SHALL return a new `list`
(or `dict`) value and SHALL NOT mutate the receiver or the source array. A `list` SHALL expose its
underlying plain JavaScript array via `.to_array()` and SHALL coerce to that array through
`toJSON()` (so `json()`/`JSON.stringify` serialize a plain array). It SHALL support element access
via `.get(i)`/`.at(i)`, a length via `.len()`, and iteration via `Symbol.iterator` (so `for..of` and
spread work). It SHALL NOT rely on `Proxy` for indexing (the engine removes `Proxy`).

#### Scenario: Transforms do not mutate the receiver

- **WHEN** a handler holds `l = list([3,1,2])` and calls `l.sort_by()`
- **THEN** a new `list` is returned while `l.to_array()` still equals `[3,1,2]`

#### Scenario: Unwrap to a plain array

- **WHEN** a handler calls `list([1,2]).to_array()` and `JSON.stringify(list([1,2]))`
- **THEN** it obtains the plain array `[1,2]` (JSON as `"[1,2]"`), never a wrapper object

#### Scenario: Positional access and iteration

- **WHEN** a handler calls `list(["a","b"]).get(1)`, `list(["a","b"]).len()`, and spreads `[...list(["a","b"])]`
- **THEN** it obtains `"b"`, `2`, and `["a","b"]` respectively

### Requirement: Field-name-first filtering, sorting, and selection (no callbacks)

`list` SHALL expose shaping verbs that take field-name strings and match-by-example objects, never
functions. It SHALL provide at least: `where(match)` (keep records where every `field: value` pair
in `match` equals the record's field), `sort_by(field, direction?)` (stable ascending sort by the
named field; `direction === "desc"` for descending), `column(field)` (a new `list` of that one
field's value from each record), `unique()` (distinct scalars by value), and `unique_by(field)`
(distinct records by the named field, keeping first occurrence). No method in this surface SHALL
accept a callback.

#### Scenario: Match-by-example filter

- **WHEN** a handler calls `list([{s:"paid"},{s:"open"},{s:"paid"}]).where({s:"paid"}).len()`
- **THEN** it obtains `2`

#### Scenario: Sort by a named field, ascending and descending

- **WHEN** a handler calls `list([{p:3},{p:1},{p:2}]).sort_by("p").column("p").to_array()` and the same with `sort_by("p","desc")`
- **THEN** it obtains `[1,2,3]` and `[3,2,1]` respectively

#### Scenario: Select one column as a flat list

- **WHEN** a handler calls `list([{email:"a"},{email:"b"}]).column("email").to_array()`
- **THEN** it obtains `["a","b"]`

#### Scenario: Distinct values and distinct records by field

- **WHEN** a handler calls `list([1,2,2,3]).unique().to_array()` and `list([{id:1},{id:1},{id:2}]).unique_by("id").len()`
- **THEN** it obtains `[1,2,3]` and `2`

### Requirement: group_by bridges list to dict

`list` SHALL provide `group_by(field)` that returns a `dict` whose keys are the stringified values
of the named field and whose values are `list`s of the records sharing that key, preserving input
order within each group.

#### Scenario: Group records into a dict of lists

- **WHEN** a handler calls `list([{r:"x",n:1},{r:"y",n:2},{r:"x",n:3}]).group_by("r")`
- **THEN** it obtains a `dict` where `get("x")` is a `list` of the two `r:"x"` records and `get("y")` is a `list` of the one `r:"y"` record

### Requirement: Exact-Decimal aggregates over a named column

`list` SHALL provide column aggregates that never float-sum currency: `sum(field)`, `avg(field)`,
`min(field)`, and `max(field)` SHALL return an exact `Decimal` value (composing over the injected
`Decimal` global), and `count()` SHALL return a plain `number`. Non-numeric or absent field values
SHALL be skipped. For an empty column (no numeric values), `sum` SHALL return `Decimal(0)` and
`avg`/`min`/`max` SHALL return `null`.

#### Scenario: Sum a currency column exactly

- **WHEN** a handler calls `list([{t:"0.1"},{t:"0.2"}]).sum("t").toString()`
- **THEN** it obtains the exact string `"0.3"` (never `0.30000000000000004`)

#### Scenario: Average, min, and max return Decimals

- **WHEN** a handler calls `list([{n:1},{n:2},{n:3}]).avg("n").toString()`, `.min("n").toString()`, `.max("n").toString()`
- **THEN** it obtains `"2"`, `"1"`, and `"3"` respectively

#### Scenario: Count returns a plain number and empty aggregates report absence

- **WHEN** a handler calls `list([1,2,3]).count()` and `list([]).avg("n")`
- **THEN** it obtains the number `3` and `null` respectively

### Requirement: first and last accessors

`list` SHALL provide `first()` and `last()` returning the first and last elements respectively, and
`null` when the list is empty.

#### Scenario: First and last of a non-empty and empty list

- **WHEN** a handler calls `list([10,20,30]).first()`, `list([10,20,30]).last()`, and `list([]).first()`
- **THEN** it obtains `10`, `30`, and `null` respectively

### Requirement: Caller-controlled allocation is bounded

Any `list` operation whose produced size is caller-controlled SHALL cap the produced length before
allocating, in the spirit of the engine's `max_*_bytes` limits, so that a single call cannot exhaust
the isolate's memory.

#### Scenario: Oversize expansion is refused rather than allocated

- **WHEN** a handler requests an operation whose produced length would exceed the capability's output cap
- **THEN** the call throws rather than allocating unboundedly
