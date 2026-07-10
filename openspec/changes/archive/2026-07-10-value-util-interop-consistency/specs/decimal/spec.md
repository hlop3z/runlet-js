# decimal Specification

## ADDED Requirements

### Requirement: snake_case naming (no aliases)

Every author-facing method on `Decimal` SHALL be named in snake_case (`is_zero`, `is_negative`,
`to_number`, etc.). The former camelCase spellings `isZero`, `isNegative`, and `toNumber` SHALL NOT
be available — they are removed, not retained as aliases, so the surface has exactly one canonical
name per operation. The JS-runtime protocol hooks the engine invokes by fixed name (`toString`,
`toJSON`, `valueOf`) SHALL keep their JS spelling.

#### Scenario: snake_case is canonical

- **WHEN** a handler calls `Decimal("5").is_zero()` and `Decimal("5").to_number()`
- **THEN** both resolve to the snake_case methods and return the expected results

#### Scenario: Removed camelCase alias is absent

- **WHEN** a handler calls the legacy `Decimal("5").isZero()`
- **THEN** it throws a `TypeError` (the camelCase alias no longer exists); the handler uses `is_zero()` instead

#### Scenario: Protocol hooks keep JS spelling

- **WHEN** the engine serializes a decimal via `JSON.stringify`
- **THEN** it invokes `toJSON` (JS spelling), which returns the exact string value

## REMOVED Requirements

### Requirement: snake_case naming with deprecated aliases

**Reason**: The one-release deprecation window for the camelCase aliases (`isZero`, `isNegative`,
`toNumber`) is closed. Retaining them left two spellings per operation, contradicting the
"one canonical, IntelliSense-discoverable form" surface rule. Replaced by **snake_case naming (no
aliases)**, which drops the aliases entirely.

**Migration**: Replace any remaining `Decimal(...).isZero()` / `.isNegative()` / `.toNumber()` calls
with `.is_zero()` / `.is_negative()` / `.to_number()`. The snake_case forms have been available since
the aliases were introduced, so this is a mechanical rename.
