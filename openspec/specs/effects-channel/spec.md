# effects-channel Specification

## Purpose

The effects channel is the `emit(kind, value)` seam through which a handler proposes structured,
tagged side-effect intents without the core interpreting them. Each call records an ordered
`{ kind, value }` entry surfaced on the execution outcome (the engine disposes; the logic
proposes). `kind` is a first-class governance/routing tag; `value` is opaque JSON. This
capability defines the `emit` contract, ordering, failure-survival, bounding, and the
opaque-value / governance-tag split. Rationale: `docs/design/` and `CLAUDE.md`.

## Requirements

### Requirement: emit takes a required kind tag and an opaque value

The `emit` global SHALL take the form `emit(kind, value)`, where `kind` is a required
non-empty string and `value` is any JSON value (opaque to the core). A call whose `kind` is
missing, empty, not a string, or exceeds the configured length bound SHALL fail deterministically
(the script observes an error) and SHALL NOT record an effect. The core SHALL NOT interpret the
meaning of `kind` — it is a routing/governance tag surfaced structurally for the consumer.

#### Scenario: A well-formed emit records a tagged effect

- **WHEN** a handler calls `emit("decided", { tier: "tier-3" })`
- **THEN** an effect `{ kind: "decided", value: { tier: "tier-3" } }` is recorded

#### Scenario: An emit with a missing or empty kind fails

- **WHEN** a handler calls `emit("", value)` or `emit(value)` with no kind
- **THEN** the call fails deterministically and no effect is recorded

### Requirement: Effects are captured in call order

Effects SHALL be captured in the order `emit` is called and surfaced on the execution outcome
as an ordered list of `{ kind, value }` entries, preserving order and duplicates.

#### Scenario: Multiple emits preserve order

- **WHEN** a handler calls `emit("a", 1)` then `emit("b", 2)` then `emit("a", 3)`
- **THEN** the outcome carries effects `[{kind:"a",value:1}, {kind:"b",value:2}, {kind:"a",value:3}]` in that order

### Requirement: Effects survive handler failure

Effects emitted before a handler throws or the execution otherwise errors SHALL still be
captured and surfaced on the (error) outcome. A partial run SHALL retain every effect emitted
up to the point of failure.

#### Scenario: A handler that emits then throws keeps its effects

- **WHEN** a handler calls `emit("finding", x)` and then throws before returning
- **THEN** the outcome is an error AND the recorded effects still include `{kind:"finding", value:x}`

### Requirement: emit calls are bounded per execution

The number of `emit` calls in a single execution SHALL be capped (the existing per-execution
`max_ops` bound). An `emit` call beyond the cap SHALL fail deterministically rather than grow
the buffer.

#### Scenario: Exceeding the emit cap fails the call

- **WHEN** a handler issues more `emit` calls than the per-execution cap allows
- **THEN** the over-limit call fails deterministically with a limit error and no further effect is recorded

### Requirement: The value is opaque; the kind is a governance seam

The core SHALL treat `value` as opaque JSON — it neither validates nor interprets its shape.
The `kind` tag SHALL be surfaced separately from `value` so that a consumer or the platform can
route, meter, or (in a later change) authorize effects by `kind` without inspecting the opaque
`value`. The core itself SHALL NOT gate or drop effects by `kind` in this change.

#### Scenario: Distinct kinds are surfaced separately for routing

- **WHEN** a handler emits `emit("email", a)` and `emit("charge", b)`
- **THEN** the effects carry `kind` values `"email"` and `"charge"` as first-class tags a consumer can route on, with `a` and `b` preserved verbatim as opaque values
