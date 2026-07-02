# amq Specification

## Purpose

The `amq` capability is the system's **messaging** capability: it exposes a message producer to
the `QuickJS` sandbox so a handler can publish messages to a broker. It supports two operator-
selected backends — `RabbitMQ` (the default) and subject-based messaging (`NATS`) — chosen by
the resource definition's `backend`. Subject-based messaging is admitted as a *backend of this
capability*, not a new top-level capability, per the capability-admission gate in
`docs-sys/rfc.md` §3.5. It follows the trusted-connection model of `db`/`mail`: the request
enables the capability by naming a logical resource in `config.io.amq`, and the broker
endpoint/credentials come from the operator's resource definition — never the request — so
there is no SSRF guard. It is **producer-side only** — publish for both backends, plus request-
reply for the `NATS` backend; **subscribe / streaming consumption is explicitly out of scope** (it
does not fit the bounded single-execution model). The JS surface is `amq.send(...)` (publish a
batch in one connection trip) and `amq.request(...)` (request-reply, `NATS` backend only), metering
message count and payload bytes. Rationale: `src/amq.rs`, `src/js/amq.js`, and `docs/08-amq.md`.

## Requirements

### Requirement: Opt-in via config.io.amq

The `amq` global SHALL be injected only when the request names at least one operator-bound
logical resource in `config.io.amq`; otherwise it does not exist in the sandbox. The request
SHALL carry no broker endpoint or credentials — only logical resource names.

#### Scenario: Capability present with a named resource

- **WHEN** a request's `config.io.amq` names an operator-bound resource
- **THEN** the handler can reference the `amq` global and call `amq.send(...)`

#### Scenario: Capability absent without a named resource

- **WHEN** a request names no resource in `config.io.amq`
- **THEN** `typeof amq === "undefined"` inside the handler

#### Scenario: Unknown resource name is rejected

- **WHEN** `config.io.amq` names a resource the operator has not bound (for this caller)
- **THEN** the request is rejected with `RESOURCE_NOT_FOUND` and the handler does not run

### Requirement: Backend selection

The capability SHALL select its messaging backend from the resource definition's `backend`, one
of `rabbitmq` (default) or `nats`. The selected backend determines how connection fields are
interpreted and which operations are available, but the JS surface for publishing (`amq.send`)
is identical across backends.

#### Scenario: Default backend

- **WHEN** the resource definition omits `backend`
- **THEN** the `rabbitmq` backend is used

#### Scenario: NATS backend selected

- **WHEN** the resource definition's `backend` is `nats`
- **THEN** the subject-based-messaging backend is used and routing keys are interpreted as subjects

#### Scenario: Unknown backend rejected

- **WHEN** an operator resource definition declares a `backend` other than `rabbitmq` or `nats`
- **THEN** the definition is rejected as invalid configuration and no request can use it

### Requirement: Trusted operator-bound broker connection

The broker connection SHALL be taken from the operator's resource definition resolved from the
logical name (`host`, `port`, `username`, `password`, `vhost`, `exchange`) with no SSRF /
private-IP guard, because the target is operator-controlled rather than script- or
caller-controlled.

#### Scenario: Connects to the configured broker

- **WHEN** the resource definition names a host, port, and credentials
- **THEN** `amq.send` opens a connection to exactly that broker and authenticates with the supplied credentials, with no host allowlist or private-IP block applied

#### Scenario: Defaults applied to omitted fields

- **WHEN** the resource definition omits `port`, `username`, `password`, `vhost`, or `exchange`
- **THEN** the defaults `5672`, `guest`, `guest`, `/`, and `""` (the default exchange) are used respectively

### Requirement: TLS via amqps

The capability SHALL connect over TLS (`amqps://`) when the resource definition sets `tls` true,
reusing the `aws-lc-rs` rustls provider, and SHALL accept an optional `ca_cert` PEM path for a
self-hosted broker with a private certificate authority.

#### Scenario: TLS connection

- **WHEN** the resource definition sets `tls` true
- **THEN** the connection to the broker is established over TLS

#### Scenario: Custom CA certificate

- **WHEN** the resource definition sets `tls` true and its `ca_cert` names a PEM file
- **THEN** that CA is used to verify the broker certificate; omitting `ca_cert` relies on the bundled webpki roots

### Requirement: Batch publish via amq.send

`amq.send` SHALL accept a list of `[routingKey, payload]` pairs and publish every message in one
connection + channel trip, returning the number of messages published. A single
`["routingKey", payload]` pair SHALL be accepted and normalized to a one-element batch. Each
`payload` is published as its JSON-serialized bytes to the configured exchange using the pair's
routing key (the queue name under the default exchange).

#### Scenario: Publish a batch

- **WHEN** the handler calls `amq.send([["user.created", {id: 1}], ["user.created", {id: 2}]])`
- **THEN** both messages are published in one trip and the call returns `2`

#### Scenario: Single-pair shorthand

- **WHEN** the handler calls `amq.send(["emails", {to: "a@b.com"}])`
- **THEN** the single message is published and the call returns `1`

#### Scenario: Empty batch rejected

- **WHEN** the handler calls `amq.send` with no messages
- **THEN** the call fails with an `amq` error indicating at least one message is required

### Requirement: NATS backend connection and publish

When the resource definition's `backend` is `nats`, the capability SHALL connect to the
definition's `host`/`port` (default port `4222`), optionally authenticating with
`username`/`password` or a `token`, optionally over TLS, and SHALL publish each `amq.send` pair
as a message whose routing key is the NATS **subject** and whose body is the payload's JSON
bytes. The `exchange` and `vhost` fields do not apply to the `nats` backend.

#### Scenario: NATS publish via amq.send

- **WHEN** the `nats` backend is selected and the handler calls `amq.send([["events.user", {id: 1}]])`
- **THEN** the message is published to subject `events.user` and the call returns `1`

#### Scenario: NATS default port and auth

- **WHEN** the resource definition omits `port` under the `nats` backend
- **THEN** port `4222` is used, and `username`/`password` or `token` (when supplied) authenticate the connection

### Requirement: NATS request-reply via amq.request

When the resource definition's `backend` is `nats`, the capability SHALL expose
`amq.request(subject, payload)` that publishes a request to `subject` and returns the first
reply's JSON body, bounded by the definition's `request_timeout_ms` (default 5000). On any
other backend `amq.request` SHALL fail with
a non-retryable `AMQ_UNSUPPORTED` error owned by the developer.

#### Scenario: Request-reply returns the reply body

- **WHEN** the `nats` backend is selected and a responder is listening on the subject
- **THEN** `amq.request(subject, payload)` returns the reply message's parsed JSON body

#### Scenario: No responder within the timeout

- **WHEN** no reply arrives within `request_timeout_ms`
- **THEN** the call fails with a retryable `AMQ_TIMEOUT` error owned by the operator

#### Scenario: Request-reply unsupported on RabbitMQ

- **WHEN** the `rabbitmq` backend is selected and the handler calls `amq.request(...)`
- **THEN** the call fails with code `AMQ_UNSUPPORTED`, non-retryable, owned by the developer

### Requirement: Producer-only — no subscribe

The capability SHALL NOT expose any subscribe, consume, or streaming-receive operation on either
backend, because an open-ended subscription does not fit the bounded single-execution model.

#### Scenario: No subscribe surface

- **WHEN** a handler inspects the `amq` global
- **THEN** it exposes only `send` (both backends) and `request` (NATS backend) and no subscribe/consume method

### Requirement: Batch size cap

The capability SHALL reject a single `send` whose message count exceeds the resource definition's `max_batch`
(default 100) with a non-retryable `AMQ_BATCH_TOO_LARGE` error owned by the developer.

#### Scenario: Over-limit batch

- **WHEN** a `amq.send` batch contains more messages than `max_batch`
- **THEN** the call fails with error code `AMQ_BATCH_TOO_LARGE` and no messages are published

### Requirement: Error behavior on publish or connection failure

On a broker connection/authentication failure the capability SHALL throw a retryable
`AMQ_CONNECTION` error owned by the operator, and on a publish/protocol failure it SHALL throw a
retryable `AMQ_ERROR`; the thrown JS error carries the structured classification the engine
branches on.

#### Scenario: Broker unreachable

- **WHEN** the broker cannot be reached or authentication fails
- **THEN** `amq.send` throws an error with code `AMQ_CONNECTION` that is retryable and owned by the operator

#### Scenario: Publish failure

- **WHEN** a message fails to publish after the connection is open
- **THEN** `amq.send` throws an error with code `AMQ_ERROR` that is retryable

### Requirement: Metering into meta.amq_requests

Each `amq.send` call SHALL count as exactly one operation against the per-execution `max_ops`
budget regardless of batch size, and SHALL record a metric — including message count, total
payload bytes, duration, and whether the batch was published — into `meta.amq_requests`. Exceeding
`max_ops` SHALL fail the call with a non-retryable `AMQ_OP_LIMIT` error owned by the developer.

#### Scenario: One op per send

- **WHEN** a handler calls `amq.send` with a multi-message batch
- **THEN** exactly one operation is charged against `max_ops` and one entry is appended to `meta.amq_requests` carrying the message count and total payload bytes

#### Scenario: Operation cap exceeded

- **WHEN** a handler's `amq.send` call would exceed `max_ops`
- **THEN** the call fails with error code `AMQ_OP_LIMIT`, which is non-retryable and owned by the developer
