## MODIFIED Requirements

### Requirement: Pure helpers always injected

The system SHALL always expose `$std.crypto` on every execution, without any configuration and
without per-operation metering, because it performs no I/O. (Date/time helpers are not part of
the crypto surface; they are provided by the always-on `$std.datetime` value-util.)

#### Scenario: Crypto available with no config

- **WHEN** a request supplies no `config.sys` block
- **THEN** `$std.crypto` is defined and callable in the handler, and there is no `$std.crypto.date`

#### Scenario: Env and secrets default to empty without config

- **WHEN** a request supplies no `config.sys` block
- **THEN** `$std.env` is an empty object and `$std.secrets` is an empty object

### Requirement: Crypto hashing and UUID

The `$std.crypto` surface SHALL provide SHA-256 and SHA-512 hashing of a string (hex-encoded)
and `uuid()` returning a fresh random v4 UUID.

#### Scenario: SHA hashing is deterministic and hex-encoded

- **WHEN** the handler calls `$std.crypto.sha256(s)` or `$std.crypto.sha512(s)` on the same string
- **THEN** it returns the same hex digest every time for that input

#### Scenario: UUID is random per call

- **WHEN** the handler calls `$std.crypto.uuid()` twice
- **THEN** each call returns a distinct UUID string

### Requirement: Crypto HMAC signing

The `$std.crypto.hmac(algo, key, msg, encoding?)` op SHALL compute an HMAC over `msg` using
`algo` of `"sha256"` or `"sha512"`, with the key being either a plain string or a secret
handle, encoded as `"hex"` (default), `"base64"`, or `"base64url"`.

#### Scenario: HMAC with a plain string key

- **WHEN** the handler calls `$std.crypto.hmac("sha256", "my-key", "msg")`
- **THEN** it returns the hex-encoded digest, and supplying `"base64"`/`"base64url"` changes only the encoding

#### Scenario: Unsupported algorithm rejected

- **WHEN** the handler calls `$std.crypto.hmac` with an `algo` other than `"sha256"` or `"sha512"`
- **THEN** the call throws a developer/script error

### Requirement: Crypto encoders

The `$std.crypto` surface SHALL provide `base64`, `base64url`, `hex`, and `url` codecs, each
with `.encode()` and `.decode()` over UTF-8 strings.

#### Scenario: Encode then decode round-trips

- **WHEN** the handler calls `$std.crypto.base64.encode(s)` then `.decode()` on the result
- **THEN** it recovers the original string, and `base64url`/`hex`/`url` behave likewise

#### Scenario: Invalid input rejected

- **WHEN** the handler decodes a value that is not valid for the codec (bad base64/hex, non-UTF-8 bytes)
- **THEN** the call throws a developer/script error

### Requirement: Env is plain operator config

When `config.sys.env` is supplied, the system SHALL expose those values at `$std.env` as
plain, readable, returnable values.

#### Scenario: Env values readable and returnable

- **WHEN** `config.sys.env` defines `{ "REGION": "us-east-1" }`
- **THEN** `$std.env.REGION` is `"us-east-1"`, a missing key is `undefined`, and the value may be returned in `data`

### Requirement: Secrets are opaque, use-not-extract handles

When `config.sys.secrets` is supplied, the system SHALL expose each secret at `$std.secrets`
as an opaque, frozen handle carrying only the secret's name; the plaintext SHALL never enter
the JS heap. The only operation that resolves a handle to its plaintext is HMAC in the key
position, whose output is a one-way digest.

#### Scenario: Coercion yields only a placeholder

- **WHEN** the handler coerces `$std.secrets.NAME` via `String(...)`, a template literal, `JSON.stringify`, or returns it in `data`
- **THEN** the result is the placeholder `"[secret:NAME]"`, never the secret's plaintext

#### Scenario: Handle usable solely as an HMAC key

- **WHEN** the handler passes `$std.secrets.NAME` as the `key` argument to `$std.crypto.hmac`
- **THEN** the digest is computed using the Rust-side plaintext, and no plaintext crosses back into JS

#### Scenario: Handle rejected by hash, encode, and HMAC message

- **WHEN** the handler passes a secret handle to `sha256`/`sha512`, any codec `encode`/`decode`, or as the HMAC `msg`
- **THEN** the call throws a developer/script error rather than echoing or transforming the plaintext

#### Scenario: Unknown secret reference rejected

- **WHEN** an HMAC call references a `key_ref` name that was not configured in `config.sys.secrets`
- **THEN** the call throws a developer/script error
