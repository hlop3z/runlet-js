# diagnostic-logging Specification

## Purpose

The sandbox has no `console`, so a script author cannot see intermediate values and a failing run
leaves no trace of *why* it behaved as it did. This capability defines the `log.*` diagnostic
channel: structured, leveled entries (message template + named properties + rendered message +
sequence number) with bound context, a cheap level floor, per-execution bounds, and a
determinism-exclusion rule. It is developer-facing, lossy, and routed to sinks by **platform
policy** the script never sees — an always-on isolated per-tenant stream plus a gateway-gated
response mirror. Rationale: `docs/design/diagnostic-logging.md`.

## Requirements

### Requirement: Structured, leveled log entries

The sandbox SHALL expose a `log` API with the levels `trace`, `debug`, `info`, `warn`, and `error`.
Each accepted call SHALL produce a structured entry carrying the level, a message template, the
named properties supplied with it, a rendered message, and a monotonic sequence number that
preserves call order. The core SHALL surface this structure but SHALL NOT interpret the meaning of a
log entry.

#### Scenario: A log call produces a structured, ordered entry

- **WHEN** a handler calls `log.info("charged {user} {amount}", { user: 42, amount: "10.00" })`
- **THEN** an entry is recorded at level `info` carrying the template `"charged {user} {amount}"`, the
  properties `{ user: 42, amount: "10.00" }`, a rendered message `"charged 42 10.00"`, and a
  sequence number greater than any earlier entry's

#### Scenario: Entries preserve call order

- **WHEN** a handler logs three times in sequence
- **THEN** the recorded entries carry strictly increasing sequence numbers in call order

### Requirement: A level floor filters below-threshold calls cheaply

The system SHALL apply a configured minimum level, ordered `trace < debug < info < warn < error`. A
call below the floor SHALL be discarded without recording an entry and without evaluating or
serializing its properties, so a suppressed call is inexpensive.

#### Scenario: A below-floor call records nothing

- **WHEN** the floor is `info` and a handler calls `log.debug("x", { costly: expensive() })`
- **THEN** no entry is recorded

#### Scenario: An at-or-above-floor call is recorded

- **WHEN** the floor is `info` and a handler calls `log.warn("careful")`
- **THEN** an entry is recorded at level `warn`

### Requirement: Bound context

The `log` API SHALL support deriving a logger with bound context (e.g. `log.with({ requestId })`).
Entries produced through a derived logger SHALL carry the bound fields merged into their properties
alongside the per-call properties.

#### Scenario: Bound context appears on derived entries

- **WHEN** a handler creates `const l = log.with({ order: 7 })` and calls `l.info("done", { ok: true })`
- **THEN** the recorded entry's properties include both `order: 7` and `ok: true`

### Requirement: Logs are bounded per execution

The number of log entries recorded in a single execution SHALL be capped, and each entry SHALL be
bounded in size. A call beyond the per-execution cap, or an entry exceeding the size bound, SHALL be
dropped or truncated deterministically rather than growing the buffer without bound.

#### Scenario: Exceeding the per-execution cap drops further entries

- **WHEN** a handler issues more `log` calls than the per-execution cap allows
- **THEN** the over-limit calls record no further entries and the execution is otherwise unaffected

### Requirement: Logs survive handler failure

Entries logged before a handler throws, or before the execution otherwise errors, SHALL still be
captured. A partial run SHALL retain every entry logged up to the point of failure.

#### Scenario: A handler that logs then throws keeps its logs

- **WHEN** a handler calls `log.info("step 1")` and then throws before returning
- **THEN** the run is an error AND the captured entries still include the `"step 1"` entry

### Requirement: Logs are outside the reproducibility contract

Logs SHALL NOT affect an execution's reproducible `data` and `effects` outputs. Under the
deterministic profile, entry ordering SHALL be provided by the deterministic sequence number and no
wall-clock-derived timing SHALL be attached; a relative timing offset MAY be attached only under the
non-deterministic profile.

#### Scenario: A deterministic run's outputs are unaffected by logging

- **WHEN** the same deterministic handler runs twice, logging on each run
- **THEN** both runs produce byte-identical `data` and `effects`, and each entry carries a sequence
  number but no wall-clock timing

#### Scenario: A non-deterministic run may carry relative timing

- **WHEN** a handler under the full (non-deterministic) profile logs during execution
- **THEN** each entry MAY carry a relative timing offset in addition to its sequence number

### Requirement: Always-on delivery to an isolated, lossy tenant stream

Captured logs SHALL be delivered to a per-tenant diagnostic stream, attributed to the trusted tenant
and correlated by trace id, independently of the `/execute` response. Delivery SHALL be non-blocking
and lossy — dropped under backpressure with a drop signal exposed — and SHALL travel a delivery path
isolated from the billing and audit event streams, so that log volume can never cause a billing or
audit event to be dropped.

#### Scenario: An executed request streams its logs to the tenant

- **WHEN** a trusted-mode request runs a handler that logs
- **THEN** the captured entries are delivered to the tenant's diagnostic stream keyed by the trusted
  tenant id and the request's trace id

#### Scenario: Log backpressure never drops billing or audit events

- **WHEN** a handler emits enough logs to saturate the diagnostic delivery path
- **THEN** diagnostic entries are dropped (and the drop signal advances) while the request's billing
  and audit events are still delivered

### Requirement: Sink selection is platform policy, never script- or caller-controlled

Which sinks receive a log entry SHALL be determined by platform/gateway policy, not by the executing
script and not by an untrusted caller. The `log` API SHALL be identical regardless of how entries are
routed.

#### Scenario: The script cannot choose or force a sink

- **WHEN** a handler logs
- **THEN** the entry is routed by policy only; the script has no means to select a sink or force
  response inclusion
