# text Specification

## Purpose

The `text` capability is the always-on, pure string value-util for the sandbox: a top-level
factory that produces immutable string values with a snake_case, business-scripting author
surface. It provides Pythonic-named renames that delegate to native JavaScript string
operations plus a small set of ERP-common shaping verbs (slugify, mask, whitespace collapse,
truncate, padding). It is pure (no I/O, no clock, no randomness, no per-operation metering) and
is injected identically under every profile. It does human-readable *shaping* and is distinct
from `$std.crypto`/codec (reversible byte encoding, hmac, uuid) and from any future
semantic-domain validation util.

## Requirements

### Requirement: Always-on injection under every profile

The system SHALL inject a top-level `text` global into every execution context unconditionally
and identically under both `Profile::Full` and `Profile::Deterministic`, because the capability
is pure — it reads no clock, no randomness, and no ambient state — and needs no capability
config. `text` SHALL be a factory usable as a callable (`text(input)`) that coerces its argument
to a string and returns an immutable `text` value.

#### Scenario: Available with no capability config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof text === "function"` and `text("abc")` returns a value with the string methods

#### Scenario: Present and identical under the deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** `text` and all of its methods are available and behave exactly as under the full profile (nothing is removed or stubbed)

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs `text` operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Immutable value with unwrap

A `text` value SHALL be immutable: every method that transforms content SHALL return a new
`text` value and SHALL NOT mutate the receiver. A `text` value SHALL expose its underlying plain
JavaScript string via a `.value` accessor and SHALL coerce to that same string through
`toString()`, `toJSON()` (so `json()`/`JSON.stringify` serialize the plain string), and
`valueOf()` for string coercion.

#### Scenario: Transforms do not mutate the receiver

- **WHEN** a handler holds `t = text("  Hi  ")` and calls `t.strip()`
- **THEN** a new `text` value equal to `"Hi"` is returned while `t.value === "  Hi  "` is unchanged

#### Scenario: Unwrap to a plain string

- **WHEN** a handler calls `text("Ac-Me").lower().value`, `String(text("Ac-Me").lower())`, and `JSON.stringify(text("Ac-Me").lower())`
- **THEN** it obtains the plain string `"ac-me"` (JSON as the quoted string), never a wrapper object

### Requirement: Pythonic-named delegation to native string semantics

`text` SHALL expose snake_case methods that rename and delegate to native JavaScript string
operations, preserving JavaScript's semantics (including UTF-16 code-unit counting and width;
the capability does not reimplement or promise Unicode code-point or grapheme semantics). The
surface SHALL include at least: `lower`, `upper`, `strip`/`lstrip`/`rstrip`,
`starts_with`/`ends_with`, `replace`, `split`/`rsplit`/`splitlines`, `count`,
`title`/`capitalize`/`swap_case`, `removeprefix`/`removesuffix`, and the character-class
predicates `is_digit`/`is_alpha`/`is_alnum`/`is_space`. Predicate methods SHALL return a boolean;
`split`/`rsplit`/`splitlines` SHALL return an array of plain strings; `count` SHALL return a
number; all other content transforms SHALL return a `text` value.

#### Scenario: Case and strip renames

- **WHEN** a handler calls `text("  Héllo  ").strip().upper().value`
- **THEN** it returns `"HÉLLO"`, matching native `trim`/`toUpperCase` semantics

#### Scenario: Prefix/suffix and predicate helpers

- **WHEN** a handler calls `text("SKU-0042").starts_with("SKU-")`, `text("SKU-0042").removeprefix("SKU-").value`, and `text("0042").is_digit()`
- **THEN** it returns `true`, `"0042"`, and `true` respectively

#### Scenario: Splitting returns plain strings

- **WHEN** a handler calls `text("a,b,c").split(",")`
- **THEN** it returns the array `["a", "b", "c"]` of plain strings, and `text("a\nb").splitlines()` returns `["a", "b"]`

### Requirement: Padding and alignment with a bounded output size

`text` SHALL provide `zfill(width)` (left-pad with `"0"`), `ljust(width, fill?)`,
`rjust(width, fill?)`, and `center(width, fill?)` producing a `text` value at least `width`
wide, delegating to native padding semantics. Because `width` is caller-controlled, the
capability SHALL cap the produced length at a fixed maximum and SHALL throw a developer/script
error when a requested width (or a `repeat`-style expansion) would exceed that cap, rather than
allocating unboundedly, in the spirit of the engine's `max_*_bytes` limits.

#### Scenario: Zero-pad a reference code

- **WHEN** a handler calls `text("42").zfill(6).value` and `text("x").rjust(5).value`
- **THEN** it returns `"000042"` and `"    x"`

#### Scenario: Oversize width is refused

- **WHEN** a handler requests a padding width that exceeds the output-size cap
- **THEN** the call throws a developer/script error instead of allocating an unbounded string

### Requirement: ERP shaping verbs

`text` SHALL provide a small set of ERP-common shaping verbs composed from pure string
primitives: `slugify()` SHALL normalize to NFD, strip combining marks, lowercase, and collapse
non-alphanumeric runs into single hyphens with no leading/trailing hyphen; `mask(opts?)`
(alias `redact`) SHALL replace all but a kept tail of characters (default keep 4, mask
character `"*"`) — a lossy display transform, never reversible encoding; `collapse()` SHALL
trim and collapse internal whitespace runs to single spaces; `truncate(limit, opts?)` SHALL
shorten to at most `limit` characters, appending an ellipsis marker when it truncates. Each
verb SHALL return a `text` value and SHALL be deterministic and locale-independent.

#### Scenario: Slugify folds diacritics

- **WHEN** a handler calls `text("  Café Ör 01! ").slugify().value`
- **THEN** it returns `"cafe-or-01"`

#### Scenario: Mask keeps a tail

- **WHEN** a handler calls `text("4111111111111234").mask().value` and `text("4111111111111234").mask({ keep: 4, char: "#" }).value`
- **THEN** it returns `"************1234"` and `"############1234"`

#### Scenario: Collapse and truncate

- **WHEN** a handler calls `text("a   b\t c").collapse().value` and `text("hello world").truncate(5).value`
- **THEN** it returns `"a b c"` and a value of at most the configured truncated length ending in the ellipsis marker

### Requirement: Author surface is declared for IntelliSense

Every public `text` method SHALL be declared in the assembled `container/types.d.ts` (via the
`base.d.ts` source) so editor autocomplete under the bundled `checkJs` config is the single
source of truth for the callable surface. The D11 golden test SHALL fail if the generated
`types.d.ts` drifts from the declared surface. Methods SHALL be exposed only as chainable
instance methods in snake_case, never duplicated as static shortcuts on the `text` factory.

#### Scenario: Types stay in sync

- **WHEN** the `text` surface changes without regenerating `container/types.d.ts`
- **THEN** the D11 golden test (`types_dts_is_up_to_date`) fails until the declaration is regenerated
