## MODIFIED Requirements

### Requirement: `$std` is the canonical namespace of built-ins

The system SHALL expose a single namespace object `$std` that contains every built-in the
sandbox provides — value-utils (`money`, `decimal`, `text`, `datetime`, `list`, `dict`,
`template`), capabilities (`io`, `http`, `s3` — subject to their existing profile/config
gating), the runtime helpers formerly under `$sys` (`crypto`, `env`, `secrets`), and the
channels (`json`, `log`, `emit`). Each built-in SHALL be defined exactly once, as a member of
`$std`; there SHALL be no independently-defined bare-global copy.

#### Scenario: Every built-in is reachable through `$std`

- **WHEN** a handler runs under `Profile::Full` with capabilities configured
- **THEN** `$std.money`, `$std.decimal`, `$std.text`, `$std.datetime`, `$std.list`,
  `$std.dict`, `$std.template`, `$std.io`, `$std.http`, `$std.s3`, `$std.crypto`, `$std.env`,
  `$std.secrets`, `$std.json`, `$std.log`, and `$std.emit` are all defined

#### Scenario: Crypto stays grouped, env/secrets hoisted

- **WHEN** the handler reads the relocated `$sys` members
- **THEN** the crypto/codec surface is grouped under `$std.crypto.*` (e.g.
  `$std.crypto.sha256`, `$std.crypto.hmac`, `$std.crypto.base64`) and the operator
  surfaces are at `$std.env` and `$std.secrets`

#### Scenario: Capability gating is unchanged, only the path moves

- **WHEN** a request does not configure the `io` capability (or runs under
  `Profile::Deterministic`)
- **THEN** `$std.io` is `undefined`, exactly as the bare `io` global was previously absent

#### Scenario: `$std.template` is a pure both-profile member, not a bare global

- **WHEN** a handler runs under either `Profile::Full` or `Profile::Deterministic`
- **THEN** `$std.template` is defined, and the bare identifier `template` is undefined (it is a
  namespace-only value-util, never mirrored to a global, consistent with `datetime`/`list`/
  `dict`/`text`)
