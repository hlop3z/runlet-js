# capability-registry Specification

## Purpose

The composable extension model for the logic host. A capability is a first-class value —
`CapabilityDef` (name + JS wrapper + editor type fragment + mandatory trust declaration + backend)
— registered on the host at construction via a builder, replacing a hard-coded capability list. The
host routes each capability call through a per-name egress mux (local backend or fallback), enforces
the sandbox invariants centrally and fail-closed, applies the SSRF guard for script-controlled
targets, excludes all I/O under the deterministic profile, and generates the editor type surface
from the registered set. Rationale: `docs/design/composable-core.md`, `CLAUDE.md`.
## Requirements
### Requirement: Capability registration at host construction

The logic host SHALL be composed at construction time from a set of capability definitions,
each carrying a unique name, a JS wrapper surface, an editor type-declaration fragment, and a
backend implementing the shared egress port. A capability name that was never registered SHALL
NOT exist in any execution context, regardless of request config.

#### Scenario: Registered capability is callable

- **WHEN** a host is built with a capability named `db` and a request declares `db` in `config.io`
- **THEN** the `db` global exists in the handler's context and its calls reach the registered backend

#### Scenario: Unregistered capability name never appears

- **WHEN** a request's `config.io` names a capability the host was not built with
- **THEN** the corresponding global is undefined (`typeof x === "undefined"`) and no backend is invoked

#### Scenario: Duplicate registration is rejected

- **WHEN** a host is built with two capability definitions sharing one name
- **THEN** host construction fails with a configuration error before serving any request

### Requirement: Capability-owned editor type surface

Each capability definition SHALL carry its own editor type-declaration (`.d.ts`) fragment
alongside its JS wrapper, so a capability cannot be added without its types. The single
`container/types.d.ts` consumed by editors SHALL be machine-assembled from an always-on core
base fragment plus the fragment of each registered capability — never hand-maintained as a
monolith — and a build-time check SHALL fail if the checked-in file diverges from what the
registered set generates.

#### Scenario: Type fragment ships with the capability

- **WHEN** the generated `container/types.d.ts` is assembled for a host
- **THEN** it contains the core base declarations plus exactly the type fragment of each registered capability, and no fragment for an unregistered capability

#### Scenario: Drift between registry and checked-in types is caught

- **WHEN** a capability's JS surface changes but its `.d.ts` fragment (or the regenerated `container/types.d.ts`) is not updated
- **THEN** the drift-guard check fails in CI rather than shipping stale autocomplete

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
egress (e.g. the broker) when one is wired. A driver-backed call whose name has neither a
local backend nor a fallback SHALL fail with `EGRESS_UNAVAILABLE`.

#### Scenario: Local and remote backends coexist

- **WHEN** a host registers an in-process backend for `db` and wires a broker fallback, and one request calls `db` and `amq`
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
- **THEN** the neutralized ambient authorities (time, randomness — the `datetime` clock and `$std` crypto entropy) are absent from the context such that a script cannot re-reach them, rather than present-but-stubbed in a way that could be un-gated by a later change

#### Scenario: In-engine capabilities are declared as mux bypasses

- **WHEN** the `http` or `s3` capability is injected (they carry their own in-engine code and do not route through the egress mux)
- **THEN** each still enforces its own trust model — `http` applies the SSRF guard, `s3` performs only signing — and both are documented in the enumerated bypass surface so the omission from central mediation is a reviewed decision, not an oversight

### Requirement: Deterministic profile excludes all I/O capabilities

Under the deterministic execution profile the host SHALL inject no registered I/O capability,
regardless of request config or registration.

#### Scenario: Deterministic run has no capability globals

- **WHEN** an invocation runs with the deterministic profile and the host has capabilities registered
- **THEN** no capability global exists in the context and no backend is reachable from the script

### Requirement: Built-in capability set

The box SHALL ship exactly three in-engine built-in capabilities: `http` (script-controlled URL,
SSRF-guarded), `s3` (pure signing), and `io` (operator-named logical egress). It SHALL NOT ship any
driver-backed capability wrapper (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`). Additional capabilities
SHALL be **user-composed** via the `CapabilityDef` mechanism, which the framework SHALL continue to
expose.

#### Scenario: Only the three primitives are present by default

- **WHEN** a request runs under `Profile::Full` with no user-composed capabilities
- **THEN** only `http`, `s3`, and `io` globals are available; `db`/`mongo`/… are `undefined`

#### Scenario: A user-composed capability is injected like a built-in

- **WHEN** the host is built with `LogicHost::builder(...).capability(def)` and the request names that
  capability's resource in `config.io`
- **THEN** the def's wrapper is injected and routes through `$std.io.call` under the same mux invariants
  (allowlist, metering, deadline, fail-closed) as a built-in

### Requirement: Three-path capability extension model

The framework SHALL support three documented extension paths, selected by trust/infrastructure need:
(a) reaching a service over the `http` capability (including an allowlisted local service);
(b) compiling a `CapabilityDef` plus an in-process `Egress` into a consumer's own binary (the
consumer holds the driver and credentials in its own process); (c) routing `$std.io.call` to an
out-of-process broker that holds all credentials (the box holds none). Path (c) SHALL additionally
support a **broker-free** resolution: an operator-declared, co-located loopback endpoint reached
box-direct (see `tenant-egress`). The framework SHALL NOT require a broker for paths (a), (b), or the
box-direct variant of (c).

#### Scenario: In-process capability needs no broker

- **WHEN** a consumer builds a host with a `CapabilityDef` backed by an in-process `Egress`
- **THEN** `$std.io.call` for that capability's name is serviced in-process, with no broker configured

#### Scenario: Broker path keeps the box credential-free

- **WHEN** a request names a resource served by a broker (`io` path)
- **THEN** the box forwards only the logical name; it holds no backend endpoint or credential

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

