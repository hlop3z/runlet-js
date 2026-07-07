# mail Specification

## Purpose

The `mail` capability lets a handler send email through an operator-supplied SMTP relay via a
`mail.send({...})` JS global. Like `db`, it follows the trusted-connection model: the request
enables the capability by naming a logical resource in `config.io.mail`, and the relay host
and credentials come from the operator's resource definition (never the request or the
script), so no SSRF / private-IP guard is applied — internal and self-hosted relays are
intended to work. Each
send is metered into `meta.mail_requests`. Rationale: `src/mail.rs`, `src/js/mail.js`,
`docs/04-mail.md`, and `CLAUDE.md`.

## Requirements

### Requirement: Opt-in injection via config.io.mail

The `mail` global SHALL be injected only when the request names at least one operator-bound
logical resource in `config.io.mail`; with no such entry the global is absent. The request
SHALL carry no relay endpoint or credentials — only logical resource names.

#### Scenario: Capability present with a named resource

- **WHEN** a request's `config.io.mail` names an operator-bound resource and the handler reads `typeof mail`
- **THEN** `mail` is defined and exposes a `send` function

#### Scenario: Capability absent without a named resource

- **WHEN** a request names no resource in `config.io.mail` and the handler reads `typeof mail`
- **THEN** the result is `"undefined"`

#### Scenario: Unknown resource name is rejected

- **WHEN** `config.io.mail` names a resource the operator has not bound (for this caller)
- **THEN** the request is rejected with `RESOURCE_NOT_FOUND` and the handler does not run

### Requirement: Trusted operator-bound connection

The relay connection SHALL be built from the operator's resource definition resolved from the
logical name (`host`, `port`, `user`, `password`, `tls`, `from`, `max_recipients`,
`timeout_ms`) with no SSRF or private-IP block applied to the target host.

#### Scenario: Connect to operator-named host

- **WHEN** the resource definition's `host` names an internal or private relay
- **THEN** the connection is attempted without host allowlisting or private-IP rejection

#### Scenario: Transport security mode

- **WHEN** the resource definition's `tls` is `"starttls"`, `"wrapper"`, or `"none"`
- **THEN** the transport uses STARTTLS, implicit-TLS (SMTPS), or plaintext respectively, defaulting to `"starttls"` on port `587`

#### Scenario: Optional authentication

- **WHEN** the resource definition's `user` is empty
- **THEN** the connection is made without SMTP authentication

### Requirement: Send JS surface

The `mail` global SHALL expose `mail.send(opts)` accepting `from`, `to`, `cc`, `bcc`,
`reply_to`, `subject`, `text`, and `html`; `to`/`cc`/`bcc` each accept a single address string
or an array of address strings, and `from` defaults to the resource definition's `from` when
omitted.

#### Scenario: Single or list recipients

- **WHEN** `to` (or `cc`/`bcc`) is given as a single string or as an array of strings
- **THEN** each is normalized to a recipient list

#### Scenario: Default from address

- **WHEN** `send` is called without a `from`
- **THEN** the resource definition's configured `from` is used as the From address

#### Scenario: Body selection

- **WHEN** a send provides `text`, `html`, or both
- **THEN** a text-only, html-only, or `multipart/alternative` message is built respectively

### Requirement: Recipient validation

A send SHALL require at least one recipient and SHALL reject sends whose total recipients
(`to` + `cc` + `bcc`) exceed the resource definition's `max_recipients` (default 50), and SHALL validate every
address.

#### Scenario: No recipients

- **WHEN** a send supplies no `to`, `cc`, or `bcc` addresses
- **THEN** `mail.send` throws an error (recipient required)

#### Scenario: Too many recipients

- **WHEN** the total recipient count exceeds `max_recipients`
- **THEN** `mail.send` throws an error reporting the count and the cap

#### Scenario: Invalid address

- **WHEN** any `from`/`to`/`cc`/`bcc`/`reply_to` address fails to parse as a mailbox
- **THEN** `mail.send` throws an error naming the offending field and value

### Requirement: Send outcome and error classification

On success `mail.send` SHALL return `{ accepted, response }`; on failure it SHALL throw a tagged
capability error whose code reflects the SMTP reply class — `MAIL_TRANSIENT` (4xx, retryable),
`MAIL_PERMANENT` (5xx, not retryable), or `MAIL_ERROR` (connect/TLS/usage, retryable).

#### Scenario: Accepted send

- **WHEN** the relay accepts the message
- **THEN** `mail.send` returns `{ accepted: true, response: <server reply line> }`

#### Scenario: Transient vs permanent failure

- **WHEN** the relay rejects with a 4xx reply versus a 5xx reply
- **THEN** the thrown error carries code `MAIL_TRANSIENT` (retryable) versus `MAIL_PERMANENT` (not retryable)

#### Scenario: Connection or usage failure

- **WHEN** the failure is a connect/TLS/IO error or a payload/validation error
- **THEN** the thrown error carries the fallback code `MAIL_ERROR`

### Requirement: Metering and operation cap

Each send SHALL be recorded into `meta.mail_requests` with its recipient count, serialized byte
size, and accepted flag, and SHALL be subject to the per-execution `max_ops` budget.

#### Scenario: Send recorded in meta

- **WHEN** a handler performs a `mail.send`
- **THEN** an entry appears in `meta.mail_requests` carrying `recipients`, `bytes`, and `accepted`

#### Scenario: Operation budget exhausted

- **WHEN** a send would exceed the per-execution `max_ops` budget
- **THEN** the call fails with code `MAIL_OP_LIMIT` (not retryable)
