## MODIFIED Requirements

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
- **THEN** the def's wrapper is injected and routes through `io.call` under the same mux invariants
  (allowlist, metering, deadline, fail-closed) as a built-in

## ADDED Requirements

### Requirement: Three-path capability extension model

The framework SHALL support three documented extension paths, selected by trust/infrastructure need:
(a) reaching a service over the `http` capability (including an allowlisted local service);
(b) compiling a `CapabilityDef` plus an in-process `Egress` into a consumer's own binary (the
consumer holds the driver and credentials in its own process); (c) routing `io.call` to an
out-of-process broker that holds all credentials (the box holds none). Path (c) SHALL additionally
support a **broker-free** resolution: an operator-declared, co-located loopback endpoint reached
box-direct (see `tenant-egress`). The framework SHALL NOT require a broker for paths (a), (b), or the
box-direct variant of (c).

#### Scenario: In-process capability needs no broker

- **WHEN** a consumer builds a host with a `CapabilityDef` backed by an in-process `Egress`
- **THEN** `io.call` for that capability's name is serviced in-process, with no broker configured

#### Scenario: Broker path keeps the box credential-free

- **WHEN** a request names a resource served by a broker (`io` path)
- **THEN** the box forwards only the logical name; it holds no backend endpoint or credential
