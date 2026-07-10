## MODIFIED Requirements

### Requirement: Non-blocking, fail-open emission

Event emission SHALL NOT unboundedly block or fail the request path. The system SHALL maintain two
isolated bounded channels: a **precious** channel carrying `usage` and `audit` events, and a
**separate** lossy channel carrying diagnostic `log` events, so log volume can never induce a
usage/audit loss.

For the precious usage/audit channel, emission SHALL be *bounded block-with-timeout*: the emitter
SHALL briefly wait for channel capacity up to a small, configurable timeout, and SHALL count a
dropped event ONLY if the channel is still saturated when that timeout expires. A usage/audit event
SHALL NOT be dropped merely because the channel is momentarily full. The precious channel SHALL be
sized generously so that, under normal load, the timeout path is effectively unreachable. Every
dropped usage/audit event SHALL increment a dropped-events counter that is intended as an SLO /
alerting signal (a genuine revenue or compliance leak), not a routine occurrence.

For the diagnostic `log` channel, emission SHALL remain best-effort: under backpressure log events
are dropped immediately (not awaited) and a separate log-dropped counter is incremented.

When event emission is disabled or unconfigured, request handling SHALL be unaffected and no events
SHALL be produced. On graceful shutdown, the precious usage/audit channel SHALL be flushed before
exit (the writer drains its remaining buffered events); the log channel is flushed on a best-effort
basis.

Delivery remains at-least-once: the per-event `event_id` is the idempotency key a downstream
consumer uses to deduplicate. No ordering or exactly-once guarantee is provided.

#### Scenario: Momentarily full precious channel does not drop a usage/audit event

- **WHEN** the usage/audit channel is transiently full but drains within the block-with-timeout window
- **THEN** the usage/audit event is enqueued (not dropped) once capacity frees, and the request path is not blocked beyond the small timeout

#### Scenario: Sustained saturation drops and loudly counts a usage/audit event

- **WHEN** the usage/audit channel is still saturated when the block-with-timeout expires
- **THEN** the event is dropped, the usage/audit dropped-events counter increments, and that counter is exposed as an SLO/alerting signal

#### Scenario: Log backpressure never drops a usage/audit event

- **WHEN** the diagnostic `log` channel is saturated and dropping log events
- **THEN** usage/audit events on their separate channel are unaffected and the log-dropped counter (not the usage/audit counter) increments

#### Scenario: Request path is never blocked unboundedly

- **WHEN** the usage/audit channel is saturated for longer than the configured timeout
- **THEN** the emitter stops waiting after the timeout, drops-and-counts the event, and the request completes

#### Scenario: Graceful shutdown flushes the precious channel

- **WHEN** the box shuts down gracefully with buffered usage/audit events still in the precious channel
- **THEN** the writer drains those buffered events to the output stream before the process exits

#### Scenario: Disabled emission is inert

- **WHEN** event emission is disabled in config
- **THEN** requests behave exactly as before and no events are produced
