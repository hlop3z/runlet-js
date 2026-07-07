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

### D1 — Composition is compile-time, via a `LogicHost` builder that erases to a concrete type
Devs assemble their own binary: `LogicHost::builder(cfg).capability(def).fallback_egress(e).build()`.
`.build()` MUST collapse to a single non-generic `LogicHost` — capabilities are held as
`Vec<CapabilityDef>` / a `HashMap<name, Arc<dyn Egress>>`, not accumulated into a type parameter
per capability. This is the tower/axum lesson: a `LogicHost<Db, Http, Mail, …>` generic leaks the
full capability list into every consumer's signatures, making add/remove-a-capability a breaking
change for downstream code and exploding the published type surface. The builder may be generic
internally; the built value is monomorphic. (We already erase the backend via `Arc<dyn Egress>`;
this states the invariant so a future refactor doesn't reintroduce capability-typed generics.)
*Alternatives*: runtime dylib plugins (breaks the lint gauntlet/supply-chain/one-crypto-stack
story); config-file-driven loading (capabilities are code + trust policy, not config);
auto-registration via `inventory`/`linkme` (rejected — a security choke point wants the capability
set explicit and greppable, not assembled by life-before-main linker tricks).

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

### D9 — The mux fails closed on its own internal errors; the bypass surface is enumerated
Reference-monitor discipline: complete mediation is only complete if the mediator never falls
through. If metering, the deadline-clock read, or trust-policy evaluation errors or panics, the
call is **denied**, not executed — the reflexive "don't break the customer, fail open" instinct is
the wrong default for a security boundary. Corollary: the authorities that legitimately do *not*
pass the mux (in-engine `http`/`s3`, ambient clock/RNG/exit) are the real unmediated surface, so
they are enumerated as a reviewed list rather than discovered later. `Profile::Deterministic` must
*remove* those ambient imports, not leave them registered-but-gated (WASI lesson: gated-but-present
authority gets un-gated by a later refactor). *Alternative*: best-effort mediation with fail-open on
enforcement errors — rejected; it converts an enforcement bug into a silent capability escape.

### D10 — Action tokens are a typed contract in the frozen crate, not free strings
The snake_case action token (first arg to `__<cap>`) is today kept in sync by hand between
`js/<cap>.js` and the Rust dispatch `match`. Splitting wrappers into `runlet-caps` while backends
live in the `fabricd` repo widens that string contract across three release cadences — a renamed
verb becomes a runtime `unknown action`, not a compile error. Scope-preserving fix: define the
action tokens as a shared `const`/enum **within this repo** (`runlet-caps`, or a small shared module
it and the dispatch both import) so the wrapper and the Rust dispatch reference one source of truth
and a rename fails to compile on the box side. The **cross-repo** seam to `fabricd` cannot be
compile-checked from here regardless — it stays governed by the existing `runlet-wire` string
protocol plus the D6 fixture test asserting tokens against `fabric-backends`' names. (Promoting the
enum into `runlet-wire` itself would give fabricd compile-time sharing too, but that touches the
frozen contract crate — out of scope for this change; note it as a candidate for a future
coordinated `runlet-wire` bump.) Keeps the preset's hand-authored `types.d.ts` story (D6) intact —
only the internal token identity is typed, not the JS surface.
*Alternative*: keep the fixture test alone — weaker; it catches drift at test time, not at compile
time, and not at all between the wrapper and its own dispatch.

### D11 — Type surface travels with the capability; `container/types.d.ts` is generated + golden-tested
The editor-facing `types.d.ts` is today a hand-maintained monolith — the tooling-layer twin of the
hard-coded `inject_apis` list this change removes, and a wall a third-party capability author cannot
compose against. So the `.d.ts` decomposes exactly like the `.js`: `CapabilityDef` gains a
`types: &'static str` field (`include_str!("js/<cap>.d.ts")`, sibling to `js_wrapper`) — a wrapper
cannot be added without its type fragment. Core keeps a `base.d.ts` for the always-on, non-capability
surface (`json`/`Handler`/`Decimal`/`$`/`$sys`). A generator concatenates `base.d.ts` + each
**registered** def's fragment into `container/types.d.ts`; a golden `#[test]` regenerates and diffs so
the checked-in file cannot rot — the same drift-guard discipline as the D10 action-token fixture, and
the registry stays the single source of truth for injection *and* IntelliSense. This refines D6/D10's
"hand-authored `types.d.ts`" note: still hand-authored **per fragment**, but machine-assembled, not a
monolith. Collision policy: fragments share one global TS namespace, so each fragment prefixes its
interface names by convention (keeps autocomplete flat — `db.query(...)`, not `Db.Row`); third-party
authors own their own naming. `meta.io` typing (D8): `base.d.ts` declares `meta.io: Record<string,
IoMetric>`; a fragment MAY narrow its own entry — start generic.
*Alternative*: keep the monolith and ask extension authors to fork it — rejected; it blocks the
composable-author story this whole change exists to enable.

### D12 — Rename the script-facing HTTP global `api` → `http`
The internals are already `http` end to end — module `http.rs`, native hook `__http`, cargo feature
`http`; only the JS global `globalThis.api` and its wrapper file `js/api.js` still say `api`. Finish
the rename: `globalThis.api` → `globalThis.http`, `js/api.js` → `js/http.js`, its D11 `.d.ts`
fragment, and `HttpClient`/`declare const api` → `declare const http`. Why: accuracy (it does HTTP),
discoverability (callers look for `http`/`fetch`/`request`, never `api`), and uniformity — every other
global names its resource/protocol (`db`/`mongo`/`mail`/`s3`/`redis`/`amq`/`auth`); `api` was the lone
vague outlier. Ride the D8 clean-break window: pre-publish, single known consumer, no alias — free now,
a breaking change with real users post-publish. `http` stays in core (D7); `config.allowed_hosts`
gating name is unchanged; no behavior change, a label only.
*Alternative*: rename to `fetch` — rejected; `fetch` implies the web-standard Promise/`Request`/
`Response` shape, but ours is method-based (`.get`/`.post`/`.request`). *Alternative*: keep `api` for
familiarity — rejected; there is no installed base to be familiar with yet, and the inconsistency is
permanent once published.

### Decision: Capability registry / plugin composition — Build (hand-written builder)

- **Status**: approved
- **Why**: Extism/Wasmtime component model solve *runtime untrusted third-party plugin loading* — a
  Non-Goal here; adopting one adds a second sandbox + crypto + supply-chain stack beside QuickJS for
  no benefit. Our extension unit is trusted compile-time Rust carrying a trust policy.
- **Considered**: Adopt Extism / Wasmtime (mature, but the wrong problem — runtime WASM artifacts).
- **Isolation**: `LogicHost::builder` + `CapabilityDef` composed over the existing
  `runlet_wire::Egress` trait; no new runtime, no new crypto stack.

### Decision: SSRF guard the mux enforces for ScriptControlled caps — Extend reqwest + Adopt `ip_network`

- **Status**: approved
- **Why**: the framework applies the audited reqwest-resolver connect-pinning plus `ip_network`
  classification; the full build-vs-adopt evaluation and adapter boundary are recorded in the
  `ssrf-guard-hardening` change (this change consumes that guard, it does not re-decide it).
- **Considered**: Adopt `agent-fetch` (immature); keep a fully hand-rolled classifier.
- **Isolation**: the shared `ssrf.rs` module the mux calls for every `ScriptControlled` def.

## Risks / Trade-offs

- [Cross-capability confused deputy — accepted residual risk] → the per-capability trust model
  (D4) is individually correct and jointly bypassable: a script can read a secret via an
  `OperatorSupplied` capability (`db`) and exfiltrate it via a `ScriptControlled` one (`http`),
  because all capabilities share one JS heap that re-ambient-izes authority. No type prevents this;
  it is the same trust the operator already extends by injecting the credential. Documented as a
  known residual risk, not a solvable framework property — cross-capability taint tracking is a
  separate, much larger proposal if ever warranted.

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
