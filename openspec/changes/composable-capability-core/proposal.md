# Proposal: composable-capability-core

## Why

runlet-core still carries six built-in capability modules (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) that, post egress-split, are hollow — each is a compile-time-baked JS wrapper behind a vestigial cargo feature, hard-wired into `engine.rs::inject_apis`, with its name frozen into `ExecMetrics` struct fields and the request/response envelope. This blocks the crate's actual goal: a minimal, publishable logic host that Rust developers extend with their own capabilities (direct Postgres, NATS, anything) by composition, without forking core. The refactor must land **before** `runlet-core` is published — the builder/registry is the public API we would be committing to.

## What Changes

- **Capability registry + builder**: a capability becomes a first-class value (`CapabilityDef`: name + JS wrapper source + trust declaration + gate) registered on `LogicHost` via a builder, replacing the hard-coded `inject_apis` list.
- **Egress mux**: the single `Option<Arc<dyn Egress>>` slot becomes a per-capability-name routing table — each registered capability binds its own backend (in-process or remote), with an optional fallback egress (the fabricd sidecar) for names not bound locally. `runlet_wire::Egress` itself is unchanged.
- **Mandatory trust declaration**: every `CapabilityDef` must declare `OperatorSupplied` or `ScriptControlled(SsrfPolicy)` targets; script-controlled capabilities get the SSRF guard applied by the framework, not by author discipline.
- **Standard capabilities move out of core** into a new `runlet-caps` preset crate (pure data: the six JS wrappers as `CapabilityDef`s, no drivers). The `runlet` bin composes the preset — stock binary behavior unchanged.
- **BREAKING**: the six vestigial cargo features (`db`, `mongo`, `mail`, `redis`, `amq`, `auth`) are removed from `runlet-core`; only `http` and `s3` remain (the two in-engine capabilities that carry real code). `http`/`s3` stay in core.
- **BREAKING**: per-capability response metrics move from fixed `meta.<cap>_requests` fields to a dynamic `meta.io.<name>` map keyed by capability name (custom capabilities meter identically to standard ones). Pre-publish, single known consumer — break clean, no alias window.
- Sandbox invariants (per-request opt-in, op limits/metering, deadline propagation, error taxonomy, `Profile::Deterministic` injecting no I/O) are enforced centrally by the mux for **all** capabilities, built-in or dev-registered — extensions cannot opt out. The mux **fails closed**: an error in its own enforcement (metering, deadline-clock, trust-policy eval) denies the call rather than falling through to the I/O. Authorities that legitimately do not pass the mux (in-engine `http`/`s3`, ambient clock/RNG/exit) are **enumerated** as a reviewed bypass surface, and `Profile::Deterministic` *removes* the ambient ones rather than leaving them registered-but-gated.

## Capabilities

### New Capabilities
- `capability-registry`: the composable extension model — `CapabilityDef` registration, per-name egress routing with fallback, mandatory trust declaration, per-request gating, deterministic-profile exclusion, and uniform metering/error mapping for registered capabilities.

### Modified Capabilities
- `execution`: response `meta` per-capability metrics change shape (fixed `<cap>_requests` fields → dynamic `meta.io.<name>` map); `config.io` gating is defined generically over registered capability names instead of a closed six-name set.

## Impact

- **Crates**: `runlet-core` (registry/builder/mux, module deletions, feature removal), new `runlet-caps` (preset), `runlet` bin (compose preset; handler `RequestConfig`/`Meta` dynamization). `runlet-wire` and the fabricd repo are untouched.
- **API**: `LogicHost` construction becomes builder-based; `/execute` response envelope `meta` shape changes (**BREAKING** above).
- **Tests/docs**: `tests/test_simple.py` meta assertions, `container/types.d.ts` meta typings, README/CLAUDE.md capability-pattern sections, `scripts/sweep.sh` feature matrix shrinks to `http`/`s3`.
- **Sequencing**: prerequisite for publishing `runlet-core`/`runlet-wire`; the `batch-execute-endpoint` change builds on the new `meta` shape and goes second.
