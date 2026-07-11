## ADDED Requirements

### Requirement: `$std` members are materialized lazily on first access

The system SHALL materialize each `$std` value-util member (`decimal`, `money`, `sys`/`crypto`/
`env`/`secrets`, `datetime`, `text`, `list`, `dict`, `template`, `check`) **on first access within a
request**, rather than eagerly constructing every member before the handler runs. Materialization
SHALL be observationally transparent: a member accessed for the first time SHALL be
indistinguishable from one that was eagerly built — same value, same type, same frozen state, same
profile/config gating. A member SHALL be built **at most once per request** and cached, so repeated
accesses and the projected bare global resolve to the *same* object reference. Members never touched
by a handler SHALL NOT be built. The channels and per-request members that carry per-invocation state
(`json`, `log`, `emit`) MAY remain eager.

#### Scenario: A touched member is built; an untouched member is not

- **WHEN** a handler under `Profile::Full` reads `$std.money` but never references `$std.template`
- **THEN** `$std.money` returns the fully-built money factory (identical to eager injection), and
  `$std.template`'s builder was never invoked for that request

#### Scenario: Identity is preserved under lazy build

- **WHEN** a handler accesses the bare global `$` and then `$std.money` (in either order)
- **THEN** `$ === $std.money` is `true` — both resolve to the same single instance built once for
  that request

#### Scenario: A member is built at most once per request

- **WHEN** a handler accesses the same member (e.g. `$std.list`) multiple times
- **THEN** the member is constructed only on the first access and the same reference is returned
  every subsequent access

#### Scenario: Lazy materialization is transparent to existing behavior

- **WHEN** a handler exercises any `$std.<member>` that was previously eagerly injected
- **THEN** the member behaves identically to the pre-change eager build (same methods, same
  results), and no handler-visible timing or ordering guarantee changes

### Requirement: The lazy builder constructs the determinism-pruned variant directly

Under `Profile::Deterministic` the system SHALL have the lazy builder construct the already-pruned
form of a member on first access, rather than building the full member and deleting ambient
authorities afterward. Every prunable authority (`datetime.now`, `crypto.uuid`, `Math.random`, and
the no-argument `Date()` clock read) SHALL be absent via every access path in a deterministic
request, exactly as today, with no un-pruned alias ever materialized.

#### Scenario: Deterministic lazy build omits the clock

- **WHEN** a handler under `Profile::Deterministic` first accesses `$std.datetime`
- **THEN** the returned object has no `now` member (and `$std.crypto.uuid`, `Math.random`, and
  no-argument `Date()`/`new Date()` remain unavailable), just as if the eager prune had run

#### Scenario: Full-profile lazy build keeps ambient authorities

- **WHEN** the same handler runs under `Profile::Full`
- **THEN** `$std.datetime.now` and `$std.crypto.uuid` are present, confirming the builder's pruning
  is gated on the profile, not unconditional

## MODIFIED Requirements

### Requirement: `$std` is frozen and its globals locked after pruning

The system SHALL guarantee that a handler cannot mutate, replace, or extend any `$std` member, add
or remove a member of `$std`, nor reassign an exposed global. Because members are materialized
lazily, the freeze SHALL be applied **per member at the moment it is built** (a member is deep-frozen
before its reference is ever handed to the handler), and the `$std` container's member set SHALL be
locked (non-configurable, non-writable member slots) before `handler` runs so members cannot be
added, removed, or replaced. Under `Profile::Deterministic` the pruned variant is what the builder
produces, so the deletions take effect *before* that member is frozen — preserving the original
prune-before-freeze ordering within each lazy build. The exposed global bindings SHALL be locked
non-writable before `handler` runs, independent of whether their backing member has been
materialized yet.

#### Scenario: Namespace members cannot be replaced

- **WHEN** a handler assigns `$std.io = someFn` or adds `$std.newThing = 1`
- **THEN** the assignment has no effect (the member slot is locked / the built member is frozen), and
  `$std.io` retains its canonical value

#### Scenario: A lazily-built member is frozen before the handler can mutate it

- **WHEN** a handler accesses `$std.money` and then attempts `$std.money.round = 5` or to mutate a
  method
- **THEN** the mutation has no effect — the member was deep-frozen at materialization, before the
  handler received the reference

#### Scenario: Exposed globals cannot be reassigned

- **WHEN** a handler assigns `log = 5` at global scope
- **THEN** the binding does not change the injected `log`, which remains the original `$std.log`

#### Scenario: Prune happens before freeze within a deterministic lazy build

- **WHEN** a handler under `Profile::Deterministic` first accesses `$std.datetime`
- **THEN** the builder produces the variant with `now` already absent, and that pruned object is the
  one deep-frozen — no full, un-pruned `$std.datetime` is ever frozen or exposed
