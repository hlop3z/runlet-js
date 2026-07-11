# check Specification

## Purpose

The `check` capability is the always-on, pure checksum-verification value-util for the sandbox,
reached as `$std.check`. It is a factory that wraps a value and exposes scheme methods that each
answer one question — *"is this string's check digit internally consistent?"* — returning a
boolean. It covers only standards-anchored, decades-stable, registry-free checksum algorithms:
the Luhn algorithm (ISO/IEC 7812-1), the GS1 mod-10 GTIN check digit (ISO/IEC 15420), and the
raw ISO/IEC 7064 check-character systems. It is pure (no I/O, no clock, no randomness, no
per-operation metering) and is injected identically under every profile. It asserts *check-digit
consistency*, never that an entity is real or registered, and it deliberately excludes
registry-/jurisdiction-dependent validation (branded IBAN/BIC/VAT format checks, national-ID
tables) and publishing-only schemes (ISBN/ISSN).

## Requirements

### Requirement: Always-on injection under every profile

The system SHALL inject the `check` value-util as `$std.check` into every execution context
unconditionally and identically under both `Profile::Full` and `Profile::Deterministic`, because
the capability is pure — it reads no clock, no randomness, and no ambient state — and needs no
capability config. `$std.check` SHALL be a factory usable as a callable (`$std.check(input)`)
that coerces its argument to a string and returns an immutable `check` value exposing the scheme
methods. `$std.check` SHALL be reachable only through the `$std` namespace and SHALL NOT be
exposed as a bare global (it is not listed in `__stdExpose`).

#### Scenario: Available with no capability config

- **WHEN** a handler runs with no capability config in the request
- **THEN** `typeof $std.check === "function"` and `$std.check("4111111111111111")` returns a value exposing the scheme methods

#### Scenario: Present and identical under the deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** `$std.check` and all of its methods are available and behave exactly as under the full profile (nothing is removed or stubbed)

#### Scenario: Reached only through the namespace

- **WHEN** a handler references the bare identifier `check` without having declared it
- **THEN** it does not resolve to the value-util (only `$std.check` does), and no `check` bare global is defined

#### Scenario: Not metered against the operation cap

- **WHEN** a handler performs `$std.check` operations
- **THEN** those operations do not count toward `max_ops` and produce no `meta` capability metrics

### Requirement: Consistent-check-digit promise, not existence

Every `check` scheme method SHALL return a boolean asserting only that the wrapped value's check
digit is internally consistent for that scheme, and SHALL NOT assert that the value corresponds
to a real, registered, or active entity. This boundary SHALL be stated in the method's `.d.ts`
documentation. A method SHALL return `false` (never throw) when the input is not well-formed for
its scheme — for example wrong length, or containing characters outside the scheme's alphabet.

#### Scenario: Valid check digit returns true

- **WHEN** a handler calls `$std.check("4111111111111111").luhn()`
- **THEN** it returns `true`

#### Scenario: Wrong check digit returns false

- **WHEN** a handler calls `$std.check("4111111111111112").luhn()`
- **THEN** it returns `false`

#### Scenario: Malformed input returns false, not an error

- **WHEN** a handler calls `$std.check("").luhn()`, `$std.check("12x4").luhn()`, and `$std.check("123").gtin()`
- **THEN** each returns `false` and no exception is thrown

### Requirement: Luhn scheme (ISO/IEC 7812-1)

`$std.check(value).luhn()` SHALL validate the value against the Luhn mod-10 algorithm
(ISO/IEC 7812-1 Annex B): reading digits right-to-left, doubling every second digit and
subtracting 9 from any result greater than 9, the total SHALL be a multiple of 10 for a valid
value. The method SHALL accept a string of decimal digits, MAY tolerate embedded ASCII spaces and
hyphens as formatting (ignored), and SHALL return `false` for any other non-digit content or an
empty digit sequence.

#### Scenario: Known-valid and known-invalid card numbers

- **WHEN** a handler calls `$std.check("79927398713").luhn()` and `$std.check("79927398714").luhn()`
- **THEN** it returns `true` then `false`

#### Scenario: Formatting separators are tolerated

- **WHEN** a handler calls `$std.check("4111 1111 1111 1111").luhn()`
- **THEN** it returns `true` (embedded spaces are ignored as formatting)

### Requirement: GTIN scheme (GS1 mod-10 / ISO/IEC 15420)

`$std.check(value).gtin()` SHALL validate the GS1 mod-10 check digit over the GTIN family
(ISO/IEC 15420): the accepted lengths SHALL be GTIN-8, GTIN-12 (UPC-A), GTIN-13 (EAN-13), and
GTIN-14, dispatched by the digit-string length. The rightmost digit SHALL be treated as the check
digit; the preceding digits weighted alternately by 3 and 1 from the right SHALL sum with the
check digit to a multiple of 10. The method SHALL return `false` for any length outside the
accepted set, for non-digit content, or for an empty string.

#### Scenario: Valid EAN-13 and UPC-A

- **WHEN** a handler calls `$std.check("4006381333931").gtin()` and `$std.check("036000291452").gtin()`
- **THEN** each returns `true`

#### Scenario: Wrong check digit and unsupported length

- **WHEN** a handler calls `$std.check("4006381333932").gtin()` and `$std.check("12345").gtin()`
- **THEN** each returns `false`

### Requirement: ISO/IEC 7064 check-character systems

`$std.check(value).iso7064(system)` SHALL validate the value against a named ISO/IEC 7064
check-character system, computed as a pure piecewise modulus (`rem = (rem * 10 + d) % m`) so that
identifiers far longer than the exact-integer range are handled without loss. For v1 the supported
`system` SHALL include `"mod_97_10"` — the MOD 97-10 system that underlies the IBAN and LEI check
digits: alphanumeric input is mapped case-insensitively (`0`–`9` → `0`–`9`, `A`–`Z` → `10`–`35`)
into a decimal string whose value MUST be congruent to `1` modulo `97`. The `system` argument SHALL
be the documented extension point for further ISO/IEC 7064 systems, added only on concrete need.
The method SHALL return `false` for an unknown `system`, for content outside the mapped alphabet,
or for an empty value, and SHALL NOT throw.

This is the standards-only primitive by which a script validates an identifier's ISO 7064 check
digit itself — including an IBAN's, by rearranging the IBAN (its country + check characters moved
to the end) *before* the call. The method SHALL operate on the string exactly as given and SHALL
perform no country-registry, length-per-country, or IBAN-rearrangement logic of its own; the
capability SHALL NOT ship branded jurisdictional validators built on top of it.

#### Scenario: Valid MOD 97-10 payload

- **WHEN** a handler calls `$std.check("WEST12345698765432GB82").iso7064("mod_97_10")` (the GB82 IBAN with its country + check characters moved to the end)
- **THEN** it returns `true` (the mapped decimal value is congruent to `1` mod 97)

#### Scenario: Corrupted MOD 97-10 payload

- **WHEN** a handler calls `$std.check("WEST12345698765433GB82").iso7064("mod_97_10")`
- **THEN** it returns `false`

#### Scenario: Unknown system name

- **WHEN** a handler calls `$std.check("123").iso7064("mod_999")`
- **THEN** it returns `false` and no exception is thrown

### Requirement: Registry, jurisdiction, and publishing schemes are excluded

The capability SHALL NOT expose validators that depend on living registries or jurisdictional
rule tables — specifically branded `iban`/`bic`/`vat` format validation and national-ID format
tables — because such data changes over time and would break the util's determinism and
zero-maintenance guarantees. The capability SHALL NOT expose the publishing-only schemes `isbn`
or `issn`. Support for an IBAN's underlying checksum is provided only via the generic
`iso7064("mod_97_10")` primitive, which performs no country-registry or length-per-country
lookup.

#### Scenario: No branded jurisdictional methods

- **WHEN** a handler inspects a `$std.check(value)` value
- **THEN** it exposes no `iban`, `bic`, `vat`, `isbn`, or `issn` method

#### Scenario: iso7064 does not validate IBAN structure

- **WHEN** a handler calls `$std.check("WEST12345698765432GB82").iso7064("mod_97_10")` on a caller-rearranged IBAN
- **THEN** it validates only the mod-97 check arithmetic on the string as given and performs no country-length, registry, or rearrangement logic (a checksum-consistent value with a country-invalid length still returns `true`)

### Requirement: Author surface is declared for IntelliSense

Every public `check` method SHALL be declared in the assembled `container/types.d.ts` (via the
`base.d.ts` source, under `interface Std`) so editor autocomplete under the bundled `checkJs`
config is the single source of truth for the callable surface. The D11 golden test SHALL fail if
the generated `types.d.ts` drifts from the declared surface. Methods SHALL be exposed only as
chainable instance methods in snake_case on the value returned by `$std.check(...)`, never
duplicated as static shortcuts on the `$std.check` factory.

#### Scenario: Types stay in sync

- **WHEN** the `check` surface changes without regenerating `container/types.d.ts`
- **THEN** the D11 golden test (`types_dts_is_up_to_date`) fails until the declaration is regenerated
