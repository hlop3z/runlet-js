# Design: composable-capability-core

## Context

Post egress-split, runlet-core's six driver capabilities are hollow: each `<cap>.rs` only
eval's `js/<cap>.js`, every call routes through the single native `__io` hook into one
`Option<Arc<dyn Egress>>` slot (`engine.rs`), and the six names are frozen into cargo
features, `inject_apis`, `ExecMetrics` fields, and the handler's `RequestConfig`/`Meta`.
The `Egress` seam already works (engine tests inject a fake); what is missing is making the
capability itself a value a consumer composes. This must land before `runlet-core` is
published — the builder/registry is the public API we would commit to. `runlet-wire` and the
fabricd repo must stay untouched: `fabric-backends`' `*Backend`s are already in-process,
Egress-shaped plugins, and this change is what lets a custom box link them directly.

## Goals / Non-Goals

**Goals**
- A dev builds a custom box binary composing exactly the capabilities they want (direct
  Postgres, NATS, custom anything) without forking core.
- Core enforces every sandbox invariant (opt-in gating, op limits, metering, deadline,
  taxonomy, deterministic exclusion) centrally, for all capabilities, with no opt-out.
- Stock `runlet` binary keeps its exact script-facing behavior via a preset.

**Non-Goals**
- Runtime plugin loading (dylib/wasm) — different product, own proposal if ever.
- Extracting `http`/`s3` from core — possible follow-up once the registry exists.
- Schema-generated JS wrappers — a later refactor inside the preset, not framework work.
- Any change to `runlet-wire` or the fabricd repo.

## Decisions

### D1 — Composition is compile-time, via a `LogicHost` builder
Devs assemble their own binary: `LogicHost::builder(cfg).capability(def).fallback_egress(e).build()`.
*Alternatives*: runtime dylib plugins (breaks the lint gauntlet/supply-chain/one-crypto-stack
story); config-file-driven loading (capabilities are code + trust policy, not config).

### D2 — The extension unit is `CapabilityDef` in runlet-core; the backend port stays `runlet_wire::Egress`
`CapabilityDef { name, js_wrapper, trust, backend: Arc<dyn Egress> }`. The trait is already
the cross-repo contract; the def (JS injection, gating, trust) is engine policy, so it lives
in core. Consequence: `fabric-backends` needs zero changes to be used as a local plugin set.
*Alternative*: a new `Capability` trait merging wrapper+backend — rejected; it would fork the
contract fabricd already implements.

### D3 — I/O extension happens ONLY through the `__io` mux; a separate class exists for pure JS utils
The single-native-hook design is what makes invariants non-bypassable. Exposing raw
`Function::new` registration would let extensions skip metering/deadline/taxonomy. Pure,
I/O-free JS utils (the `$`/Decimal shape) may be added as a distinct, wrapper-only extension
point since they cannot do I/O by construction.

### D4 — Mandatory trust declaration on every def
`trust: OperatorSupplied | ScriptControlled(SsrfPolicy)`; the framework applies the SSRF
guard for script-controlled targets. This converts "accidentally built an SSRF hole" from a
CVE into a type error. *Alternative*: document-and-hope — rejected; it is the framework's
single most valuable opinion.

### D5 — Egress mux: per-name routing table + optional fallback
`HashMap<name, Arc<dyn Egress>>` consulted first; unbound names go to the fallback (sidecar).
Mixed topologies (db in-process, rest brokered) become trivial. `EGRESS_UNAVAILABLE` semantics
unchanged when neither exists.

### D6 — Standard six move to a `runlet-caps` preset crate (data only)
Hand-written JS wrappers + trust declarations, no drivers. The `runlet` bin composes the
preset → stock behavior unchanged. Keeps the `types.d.ts` hand-authored surface story.
*Alternative*: generate wrappers from an action schema — deferred (D3 keeps the door open).

### D7 — Kill the six vestigial cargo features; keep `http` + `s3` in core
They gate only a JS string today. Feature matrix shrinks to the two capabilities that carry
real code/deps. Pre-publish with one consumer — the cheapest moment to break. `http` stays in
core because the SSRF guard must live where it cannot be forgotten; `s3` is pure signing math.

### D8 — `meta.io.<name>` dynamic map; break clean, no alias window
Fixed `<cap>_requests` fields cannot represent dev-registered names. Pre-1.0, single known
consumer (our own tests/typings) → remove the old fields in the same change rather than
carrying a deprecation shim into the published API. Dynamization applies symmetrically to
`config.io` intake (generic name → resource list over registered names).

## Risks / Trade-offs

- [Registry indirection slows the hot path] → the mux is one hash lookup per capability call
  on top of an existing dynamic dispatch; measure with runlet-bench, but no design concern.
- [Dynamic `meta.io` breaks existing clients] → accepted intentionally (D8); tests,
  `types.d.ts`, README updated in-change; called out BREAKING in the proposal.
- [Preset drift: JS wrapper in runlet-caps vs backend action names in fabric-backends] →
  the snake_case action-token convention already governs this pair; add a preset unit test
  asserting each wrapper's action tokens against a fixture list.
- [Custom capability with OperatorSupplied trust used to proxy arbitrary script targets] →
  documentation + the D4 type forces authors to make the choice consciously; cannot be fully
  prevented at the framework level (same trust the operator already extends to db/mail).
- [Billing/usage event dims reference per-cap counts] → verify `events.rs` usage dims against
  the new metrics source in-change; the tenant-metering spec is not changed by this design.

## Migration Plan

1. Land registry + mux behind the existing behavior (standard defs registered internally) —
   engine tests keep passing before any deletion.
2. Extract `runlet-caps`, flip the `runlet` bin to compose it, delete core's six modules +
   features, dynamize `ExecMetrics`/`RequestConfig`/`Meta`.
3. Update tests/typings/docs; run the full integration suite against the sibling fabricd
   (mixed-topology test: one capability bound in-process via `fabric-backends`, rest via
   sidecar fallback).
4. Rollback: revert the change; no data or wire-protocol migration exists (response-shape
   change only).

## Open Questions

- Naming of the pure-util extension point (D3 second class) and whether it ships in this
  change or a follow-up (lean: follow-up).
- Whether `meta.io` entries should include the resolved backend kind (local vs fallback) for
  observability — useful, but leaks topology into the client envelope (lean: no; put it in
  spans/logs instead).
