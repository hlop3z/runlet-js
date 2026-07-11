# std-namespace Specification

## Purpose

The `$std` namespace is the single canonical container for every built-in the sandbox provides —
value-utils (`money`, `decimal`, `text`, `datetime`, `list`, `dict`), capabilities (`io`, `http`,
`s3`), the runtime stdlib (`crypto`, `env`, `secrets`), and the channels (`json`, `log`, `emit`).
Each built-in is defined exactly once on `$std`; a curated few (`$`, `json`, `log`, `emit`) are
mirrored to bare globals as identity-equal references driven by one declarative `EXPOSE` list.
Only pure, both-profile members may be mirrored, so the determinism prune cannot be defeated by a
surviving alias; `$std` is deep-frozen and its globals locked after the prune, before `handler`
runs. Typing is single-source (`interface Std` + derived global declares), golden-tested. Rationale:
`src/js/std.js`, `src/js/std_project.js`, `src/js/std_freeze.js`, `src/engine.rs`, `src/js/base.d.ts`.

## Requirements

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

### Requirement: Bare globals are a projection of `$std`

The system SHALL derive its bare globals from a single declarative exposure list that maps
selected `$std` members onto `globalThis`. Each exposed global SHALL be the *same object
reference* as its `$std` member. After this change the only globals the sandbox injects
SHALL be `$std`, `$` (mapped to `$std.money`), `json` (`$std.json`), `log` (`$std.log`),
and `emit` (`$std.emit`). The former bare globals `money`, `Decimal`, `datetime`, `text`,
`list`, `dict`, `io`, `http`, and `s3` SHALL NOT be injected.

#### Scenario: Exposed globals are identity-equal to their `$std` members

- **WHEN** the handler compares an exposed global to its namespace member
- **THEN** `$ === $std.money`, `json === $std.json`, `log === $std.log`, and
  `emit === $std.emit` are all `true`

#### Scenario: Former bare util globals are removed

- **WHEN** the handler references `money`, `Decimal`, `datetime`, `text`, `list`, `dict`,
  `io`, `http`, or `s3` as a bare identifier
- **THEN** each is undefined (a reference error / `typeof === "undefined"`), and the value
  is reachable only via `$std.<name>` (or `$` for money)

#### Scenario: Destructuring the namespace works

- **WHEN** the handler writes `const { io, http, list } = $std`
- **THEN** the destructured bindings are the same capability/util objects as `$std.io`,
  `$std.http`, and `$std.list`

### Requirement: Only pure members are eligible for global exposure

Because an exposed global is a second reference to a `$std` member, the exposure list SHALL
contain only members that are pure and available under both profiles. Prunable ambient
authorities (`datetime.now`, `crypto.uuid`, `Math.random`, and the no-argument `Date()`
clock read) SHALL live only at their canonical `$std` path (or JS builtin) and SHALL NEVER
be mirrored as a bare global, so the deterministic prune cannot be defeated by a surviving
reference.

#### Scenario: No prunable authority is exposed as a global

- **WHEN** the exposure list is applied
- **THEN** it maps only `money`, `json`, `log`, and `emit` (all pure) to globals, and no
  entry references `datetime.now`, `crypto.uuid`, or `Math.random`

#### Scenario: Determinism prune leaves no reachable clock or entropy

- **WHEN** a handler runs under `Profile::Deterministic`
- **THEN** `$std.datetime.now`, `$std.crypto.uuid`, and `Math.random` are undefined via
  every access path (there is no un-pruned alias), and `Date()` / `new Date()` with no
  arguments throws

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

### Requirement: Single-source typing for namespace and globals

The type definitions SHALL declare the namespace shape once (an interface `Std` plus
`declare const $std: Std`) and SHALL derive the exposed-global declarations from that same
interface, so that member access (`$std.io`), destructuring (`const { io } = $std`), and
bare-global access (`$`, `json`, `log`, `emit`) all type-check against one source and
cannot drift. The generated `container/types.d.ts` SHALL remain in sync with the authored
`base.d.ts`, enforced by the existing golden test.

#### Scenario: Both access patterns are typed off `Std`

- **WHEN** an author edits a script with the bundled `tsconfig.json` (`checkJs`)
- **THEN** `$std.money(...)`, `const { money } = $std`, and `$(...)` all resolve to the
  same `MoneyFactory` type, and an unknown member such as `$std.nope` is a type error

#### Scenario: Golden types stay in sync

- **WHEN** `base.d.ts` is changed and `container/types.d.ts` is not regenerated
- **THEN** the `types_dts_is_up_to_date` golden test fails

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
