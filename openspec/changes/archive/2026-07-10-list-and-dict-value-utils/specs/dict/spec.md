# dict Specification

## Purpose

The `dict` capability is the always-on, pure record value-util for the sandbox: a top-level factory
that wraps a single plain JSON object as an immutable value with a snake_case, **field-name-first,
callback-free** author surface. It provides a safe nested read (`get("a.b.c", default)`), key/value
reshaping (`pick`/`omit`/`keys`/`values`/`entries`), membership (`has`), and a shallow `merge`. It
is pure (no I/O, no clock, no randomness, no per-operation metering) and is injected identically
under every profile. `entries()` returns a `list`, making `list` and `dict` one matched design.

## ADDED Requirements

### Requirement: Always-on injection under every profile

The system SHALL inject a top-level `dict` global into every execution context unconditionally and
identically under both `Profile::Full` and `Profile::Deterministic`, because the capability is pure
— it reads no clock, no randomness, and no ambient state — and needs no capability config. `dict`
SHALL be a factory usable as a callable (`dict(input)`) that treats its argument as a plain object
(non-objects yield an empty record) and returns an immutable `dict` value with string keys.

#### Scenario: Available with no capability config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof dict === "function"` and `dict({a:1})` returns a value with the dict methods

#### Scenario: Present and identical under the deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** `dict` and all of its methods are available and behave exactly as under the full profile (nothing is removed or stubbed)

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs `dict` operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Immutable value with unwrap

A `dict` value SHALL be immutable: every method that transforms content SHALL return a new `dict`
(or `list`) value and SHALL NOT mutate the receiver or the source object. A `dict` SHALL expose its
underlying plain JavaScript object via `.to_object()` and SHALL coerce to that object through
`toJSON()` (so `json()`/`JSON.stringify` serialize a plain object).

#### Scenario: Transforms do not mutate the receiver

- **WHEN** a handler holds `d = dict({a:1,b:2})` and calls `d.omit("b")`
- **THEN** a new `dict` equal to `{a:1}` is returned while `d.to_object()` still equals `{a:1,b:2}`

#### Scenario: Unwrap to a plain object

- **WHEN** a handler calls `dict({a:1}).to_object()` and `JSON.stringify(dict({a:1}))`
- **THEN** it obtains the plain object `{a:1}` (JSON as `"{\"a\":1}"`), never a wrapper object

### Requirement: Safe nested read with dotted path

`dict` SHALL provide `get(path, default?)` where `path` is a dot-separated string of keys. It SHALL
walk each segment and return the value at the full path, or the supplied `default` (or `undefined`
when no default is given) if any intermediate segment is missing or is not an object.

#### Scenario: Read a present nested value

- **WHEN** a handler calls `dict({a:{b:{c:42}}}).get("a.b.c")`
- **THEN** it obtains `42`

#### Scenario: Missing path returns the default

- **WHEN** a handler calls `dict({a:{}}).get("a.b.c", "fallback")` and `dict({}).get("x.y")`
- **THEN** it obtains `"fallback"` and `undefined` respectively

### Requirement: Key/value reshaping and membership (no callbacks)

`dict` SHALL provide reshaping verbs that take field-name strings, never functions: `pick(...fields)`
(a new `dict` with only the named keys that are present), `omit(...fields)` (a new `dict` without the
named keys), `has(field)` (own-key membership as a boolean), and `merge(other)` (a new `dict` that is
a shallow last-wins merge of the receiver with `other`).

#### Scenario: Pick and omit named fields

- **WHEN** a handler calls `dict({a:1,b:2,c:3}).pick("a","c").to_object()` and `dict({a:1,b:2,c:3}).omit("b").to_object()`
- **THEN** it obtains `{a:1,c:3}` and `{a:1,c:3}` respectively

#### Scenario: Membership and shallow merge

- **WHEN** a handler calls `dict({a:1}).has("a")`, `dict({a:1}).has("z")`, and `dict({a:1,b:2}).merge({b:9,c:3}).to_object()`
- **THEN** it obtains `true`, `false`, and `{a:1,b:9,c:3}` respectively

### Requirement: keys, values, and entries bridge to list

`dict` SHALL provide `keys()`, `values()`, and `entries()`. `keys()` and `values()` SHALL each
return a `list` (of the own string keys and their values, in insertion order); `entries()` SHALL
return a `list` of `[key, value]` pairs, bridging `dict` to `list`.

#### Scenario: keys, values, and entries as lists

- **WHEN** a handler calls `dict({a:1,b:2}).keys().to_array()`, `.values().to_array()`, and `.entries().to_array()`
- **THEN** it obtains `["a","b"]`, `[1,2]`, and `[["a",1],["b",2]]` respectively
