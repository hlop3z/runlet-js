# capability-example Specification

## Purpose

A runnable, forkable reference for the bring-your-own-capability extension model. Post
byo-capabilities, composing a custom `CapabilityDef` is the whole extension story, but it shipped
with no worked example — the only `CapabilityDef::new` call in the repo was its own definition site.
This capability is that missing example: a tiny compiled crate (`examples/kv-capability`) that
composes an in-memory `kv` store against a `LogicHost` end-to-end, plus a beginner "fork-me" guide,
so a developer copies a working starting point rather than assembling one from prose. It reads like
the normal Rust a real author writes in their own crate — it does not inherit runlet-core's internal
lint gauntlet, which the extension model does not impose on users. Rationale:
`docs/03-capabilities.md`, `docs/design/composable-core.md`.

## Requirements

### Requirement: A runnable reference capability example ships in the repo

The repository SHALL contain a single, compiled, runnable example that composes a custom `CapabilityDef` against a `LogicHost` and exercises it end-to-end, so a developer can fork a working starting point rather than assembling one from prose.

The example SHALL be invocable through a documented, single command and SHALL use only the public `runlet-core` API (the `LogicHost` builder, `CapabilityDef`, `Trust`, the `Egress` port, and `Invocation`) — it MUST NOT reach into crate internals.

#### Scenario: The example runs from the documented command
- **WHEN** a developer runs the documented command (`cargo run -p kv-capability`)
- **THEN** the example builds a `LogicHost` with the `kv` capability, executes a JS handler that uses `$std.kv`, prints the resulting `{data}`, and exits successfully

#### Scenario: The example is compiled, not a prose snippet
- **WHEN** the workspace is built
- **THEN** the example crate compiles as a workspace member (so a builder-API change breaks the build in CI/Docker), reading like the normal Rust a real capability author writes — it deliberately does not inherit runlet-core's internal restriction-lint gauntlet, which the extension model does not impose on users

### Requirement: The example capability performs a get/set round-trip

The example SHALL expose an in-memory key-value capability named `kv` with exactly two actions, `get` and `set`, backed by an in-process store. A value written by `set` under a key SHALL be returned by a subsequent `get` of the same key within the same host.

#### Scenario: A written value reads back
- **WHEN** the handler calls `$std.kv.set("name", "Ada")` and then `$std.kv.get("name")`
- **THEN** `get` returns `"Ada"`

#### Scenario: A missing key reads back empty
- **WHEN** the handler calls `$std.kv.get` for a key that was never set
- **THEN** the call returns an empty/absent value (not an error, not a thrown exception)

#### Scenario: The round-trip is asserted
- **WHEN** the example runs
- **THEN** it asserts the round-trip result equals the written value, so a regression fails the run rather than passing silently

### Requirement: The example demonstrates the four in-sync pieces of a capability

The example SHALL show, in one place, the four coupled pieces of a composed capability and make their required agreement explicit: the JS wrapper (routing through `io.call('kv', action, payload)`), the Rust `Egress` backend (matching on the same action tokens), the `.d.ts` type fragment, and the `CapabilityDef` registration that binds them.

The action tokens SHALL be `snake_case` and SHALL be identical between the JS wrapper method dispatch and the backend's action match, per the project's capability-naming convention.

#### Scenario: The wrapper and backend agree on action tokens
- **WHEN** the JS wrapper issues `io.call('kv', 'get', …)` / `io.call('kv', 'set', …)`
- **THEN** the backend's action match handles exactly `get` and `set` with the same spelling, and any other action name yields a capability error rather than a panic

#### Scenario: Trust is declared operator-supplied
- **WHEN** the `CapabilityDef` is constructed
- **THEN** it declares `Trust::OperatorSupplied` (the capability has no script-controlled outbound target, so no SSRF policy applies), demonstrating the simplest trust choice

### Requirement: A fork-me guide explains how to adapt the example

The documentation SHALL include a beginner-friendly "fork-me" section that teaches the extension model from the example: the call loop (handler → wrapper → mux → backend → back), the "these four `snake_case` tokens must agree" coupling, and a short "make it yours — change these spots" list pointing at the exact edit sites.

#### Scenario: The guide names the edit sites
- **WHEN** a developer reads the fork-me section
- **THEN** it identifies each spot to change to turn `kv` into their own capability (the name, the actions, the backend body, the wrapper methods, and the `.d.ts` signatures) and links to the runnable example

### Requirement: The example does not alter shipped surface

Adding the example SHALL NOT change the shipped `runlet-core` public surface, the generated `container/types.d.ts`, or its D11 golden test. The example's `.d.ts` fragment is the developer's own, standalone illustration and MUST NOT be folded into the box's shared type file.

#### Scenario: The shipped type golden test is unaffected
- **WHEN** the change is applied and `types_dts_is_up_to_date` runs
- **THEN** it passes unchanged, because the example's `.d.ts` is not concatenated into `container/types.d.ts`
