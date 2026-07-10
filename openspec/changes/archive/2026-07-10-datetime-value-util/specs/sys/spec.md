## MODIFIED Requirements

### Requirement: Pure helpers always injected

The system SHALL always expose `$sys.crypto` on every execution, without any configuration and
without per-operation metering, because it performs no I/O. (Date/time helpers are no longer part
of `$sys`; they are provided by the always-on top-level `datetime` value-util.)

#### Scenario: Crypto available with no config

- **WHEN** a request supplies no `config.sys` block
- **THEN** `$sys.crypto` is defined and callable in the handler, and `$sys.date` does not exist

#### Scenario: Env and secrets default to empty without config

- **WHEN** a request supplies no `config.sys` block
- **THEN** `$sys.env` is an empty object and `$sys.secrets` is an empty object

## REMOVED Requirements

### Requirement: Date helpers

**Reason**: Date/time is promoted out of `$sys` into a first-class top-level `datetime` value-util
(new `datetime` capability), enriched with components, period boundaries, calendar-aware
arithmetic, comparisons, and timezone-aware views, and renamed to snake_case (`epoch_ms`).

**Migration**: Replace `$sys.date.now()` with `datetime.now()`, `$sys.date.parse(x)` with
`datetime.parse(x)` (or `datetime(x)`), and `.epochMs()` with `.epoch_ms()`. The `add`/`sub`/`diff`/
`iso`/`unix` methods keep the same names and semantics on `datetime` values.
