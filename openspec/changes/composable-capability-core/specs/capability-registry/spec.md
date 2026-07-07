# capability-registry Specification (delta)

## ADDED Requirements

### Requirement: Capability registration at host construction

The logic host SHALL be composed at construction time from a set of capability definitions,
each carrying a unique name, a JS wrapper surface, and a backend implementing the shared
egress port. A capability name that was never registered SHALL NOT exist in any execution
context, regardless of request config.

#### Scenario: Registered capability is callable

- **WHEN** a host is built with a capability named `db` and a request declares `db` in `config.io`
- **THEN** the `db` global exists in the handler's context and its calls reach the registered backend

#### Scenario: Unregistered capability name never appears

- **WHEN** a request's `config.io` names a capability the host was not built with
- **THEN** the corresponding global is undefined (`typeof x === "undefined"`) and no backend is invoked

#### Scenario: Duplicate registration is rejected

- **WHEN** a host is built with two capability definitions sharing one name
- **THEN** host construction fails with a configuration error before serving any request

### Requirement: Mandatory trust-model declaration

Every capability definition SHALL declare its target trust model: operator-supplied
(connection targets come from operator config; no SSRF restriction) or script-controlled
(targets come from script input; the framework applies the SSRF guard — host allowlist,
private/internal IP blocking). The framework SHALL enforce the declared model; capability
authors cannot opt out of the guard for script-controlled targets.

#### Scenario: Script-controlled capability gets the SSRF guard

- **WHEN** a capability declared script-controlled receives a call targeting a private/internal address not allowed by its SSRF policy
- **THEN** the call is rejected with a classified capability error before any connection is attempted

#### Scenario: Operator-supplied capability connects to configured targets

- **WHEN** a capability declared operator-supplied resolves a logical resource to an internal host from operator config
- **THEN** the connection is permitted (no SSRF block) — the db/mail trust model

### Requirement: Per-name egress routing with fallback

The host SHALL route each capability call to the backend registered for that capability's
name. Names without a locally registered backend SHALL route to the configured fallback
egress (e.g. the sidecar) when one is wired. A driver-backed call whose name has neither a
local backend nor a fallback SHALL fail with `EGRESS_UNAVAILABLE`.

#### Scenario: Local and remote backends coexist

- **WHEN** a host registers an in-process backend for `db` and wires a sidecar fallback, and one request calls `db` and `amq`
- **THEN** `db` calls are served in-process and `amq` calls are served through the fallback egress

#### Scenario: No backend and no fallback

- **WHEN** a request calls a registered capability whose name has no local backend and no fallback egress is wired
- **THEN** the call fails with error code `EGRESS_UNAVAILABLE`

### Requirement: Uniform sandbox invariants for registered capabilities

Every call through a registered capability SHALL pass the central per-request enforcement:
the op-count limit check, per-op metering drained into the response metadata, propagation of
the execution deadline to the backend, and mapping of backend errors into the classified
capability-error envelope (preserving `code`, `owner`, and `retryable`). Capability authors
SHALL NOT be able to bypass these controls.

#### Scenario: Custom capability is metered and limited

- **WHEN** a handler exceeds `max_ops` using only a dev-registered capability
- **THEN** the offending call fails with the same operation-limit error a built-in capability produces

#### Scenario: Backend error round-trips classified

- **WHEN** a registered backend returns an egress error marked retryable
- **THEN** the script observes a tagged capability error and the response error envelope carries the backend's `code` with `retryable: true`

#### Scenario: Mux fails closed on internal error

- **WHEN** the central enforcement itself errors while processing a call (e.g. the op-metering, deadline-clock read, or trust-policy evaluation fails or panics)
- **THEN** the call is denied with a classified error and no connection is attempted — the mux never falls through to executing the I/O when its own guard cannot be evaluated

### Requirement: Bounded and enumerated mux-bypass surface

The host SHALL enumerate every authority reachable from a script that is not mediated by the
capability mux — the in-engine `http` and `s3` capabilities, and ambient primitives such as the
wall clock, entropy/RNG, and process exit — as a reviewed bypass of the central enforcement. Under
the deterministic profile these ambient authorities SHALL be removed from the context, not merely
gated; a registered-but-disabled import is not acceptable.

#### Scenario: Ambient authority is removed, not gated, under deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** the neutralized ambient authorities (time, randomness, `$sys` clock/entropy) are absent from the context such that a script cannot re-reach them, rather than present-but-stubbed in a way that could be un-gated by a later change

#### Scenario: In-engine capabilities are declared as mux bypasses

- **WHEN** the `http` or `s3` capability is injected (they carry their own in-engine code and do not route through the egress mux)
- **THEN** each still enforces its own trust model — `http` applies the SSRF guard, `s3` performs only signing — and both are documented in the enumerated bypass surface so the omission from central mediation is a reviewed decision, not an oversight

### Requirement: Deterministic profile excludes all I/O capabilities

Under the deterministic execution profile the host SHALL inject no registered I/O capability,
regardless of request config or registration.

#### Scenario: Deterministic run has no capability globals

- **WHEN** an invocation runs with the deterministic profile and the host has capabilities registered
- **THEN** no capability global exists in the context and no backend is reachable from the script

### Requirement: Standard capability preset

The standard capabilities (`db`, `mongo`, `mail`, `redis`, `amq`, `auth`) SHALL be provided
as a preset of capability definitions (data only — JS wrappers + trust declarations, no
drivers) outside the core crate, and the stock server SHALL compose this preset so the
script-facing JS surface of each standard capability is unchanged.

#### Scenario: Stock binary parity

- **WHEN** the stock server executes a script using any standard capability method that worked before the registry
- **THEN** the call succeeds with the same JS surface and semantics (only the metadata shape changes, per the execution spec)

### Requirement: Minimal core build

The core crate SHALL build with default features disabled linking no network I/O, and SHALL
expose exactly two capability features: `http` and `s3` (the in-engine, code-carrying
capabilities). The former per-driver-capability features SHALL NOT exist.

#### Scenario: Deterministic-only consumer

- **WHEN** a consumer depends on the core crate with `default-features = false`
- **THEN** the build succeeds and links no HTTP client, driver, or network dependency

#### Scenario: Vestigial features removed

- **WHEN** a consumer enables a removed feature name (e.g. `db`) on the core crate
- **THEN** the build fails with an unknown-feature error (registration replaces feature gating)
