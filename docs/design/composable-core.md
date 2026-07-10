# Composable capability core

`runlet-core` is a minimal, publishable logic host. A capability is a first-class value a
consumer composes onto the host at build time — not a compile-time-baked module wired into
`engine.rs`. This is the design record for that model (change `composable-capability-core`);
the abstract behavior contract lives in
`openspec/.../specs/capability-registry/spec.md`, and the per-decision rationale (D1–D12) in
that change's `design.md`.

## The model

- **`CapabilityDef`** (`capability.rs`) — one capability as data: a unique `name`, its JS
  wrapper source, an editor `.d.ts` fragment, a mandatory `Trust` declaration, and an optional
  locally-bound `Arc<dyn Egress>` backend.
- **`LogicHost::builder(...)`** — accumulates defs + an optional fallback egress and `build()`s
  a single **non-generic** `LogicHost` (D1). Capabilities are held as data (`Vec` / `HashMap`),
  never a type parameter, so adding or removing a capability never changes a downstream
  signature. Duplicate names are rejected at build, before any request is served.
- **The egress mux** (`CapabilityRegistry::dispatch`) — every `io.call(name, action, payload)`
  routes by `name`: a locally-bound backend first, then the per-request fallback (the stock
  server's `fabricd` sidecar), then the builder-wired fallback; an unrouted registered name with
  no fallback fails `EGRESS_UNAVAILABLE`. An **unregistered** name has no JS global at all, but a
  raw `io.call` still reaches the fallback (the sidecar resolves the logical name, or nothing
  does) — the box grants no authority the operator did not wire.
- **Per-request opt-in** — a registered def's wrapper is injected only when its name is in the
  request's enabled `io` list (and only under `Profile::Full`).

## Complete mediation and the enumerated bypass surface (D9)

The mux is a reference monitor: every registered-capability call passes central enforcement —
the op-count limit, per-op metering, deadline propagation, error-taxonomy mapping, and the SSRF
guard for `ScriptControlled` targets. **It fails closed**: if its own enforcement cannot be
evaluated (a trust-policy hook errors *or panics*), the call is denied, never dispatched.

Complete mediation is only complete if the bypasses are named. The authorities reachable from a
script that do **not** pass the mux are, exhaustively:

| Authority | Where it lives | Why it bypasses the mux | Its own control |
|---|---|---|---|
| `http` | in-engine (`http.rs`), script-controlled URL | carries in-engine reqwest code, not an egress backend | the SSRF guard (allowlist + private-IP block + connect-time DNS pinning) is applied in-module and cannot be forgotten |
| `s3` | in-engine (`s3.rs`), operator-configured endpoint | pure SigV4 signing, no egress round-trip | SSRF-checks the operator endpoint host before signing; performs only signing |
| wall clock | ambient JS (`Date`, `Date.now`) | JS runtime primitive, not a capability | removed under `Profile::Deterministic` (see below) |
| entropy / RNG | ambient JS (`Math.random`), `$sys.crypto.uuid` | JS runtime primitive | removed under `Profile::Deterministic` |
| `datetime` clock | `datetime.now` | injected utility, not I/O | removed under `Profile::Deterministic` |
| process exit | not exposed | QuickJS has no host process access | n/a |

Adding any new authority to this list is a reviewed change, not an implementation detail.

### Deterministic profile *removes*, it does not gate

`Profile::Deterministic` injects **no** registered I/O capability and no `io` mux at all, and it
`delete`s the ambient nondeterministic surfaces (`Math.random`, `Date.now`, `datetime.now`,
`$sys.crypto.uuid`) rather than replacing them with throwing stubs — the WASI lesson that a
present-but-gated authority gets un-gated by a later refactor. After the sanitizer runs,
`typeof Math.random === "undefined"`: the property is gone, with no closure left holding the real
function to re-reach. The one exception is `new Date()` (no args), which is a constructor path
rather than a property; it is blocked by swapping in a constructor that throws on the zero-arg
call, so the wall clock is still structurally unreachable (there is no residual property to
reach). See `js/determinism.js` and `engine.rs`'s `deterministic_profile_removes_ambient_authority`.

## Two mechanics this change pinned down

`design.md` left two details to implementation; both are recorded here.

1. **`CapabilityDef.backend` is optional (reconciles D2 and D5).** D2 says a def carries a
   backend; D5 says names without a local backend fall through to a fallback. So the backend is
   `Option<Arc<dyn Egress>>`: present ⇒ bound in the mux routing table (in-process, e.g. a
   `fabric-backends` `*Backend`); absent ⇒ the name routes to the fallback egress. The stock
   server registers the six standard defs **without** backends and wires the sidecar as the
   per-request fallback; a custom box binds whichever backends it wants in-process and lets the
   rest fall through.

2. **`Trust::ScriptControlled(SsrfPolicy)` carries a payload target extractor (elaborates D4).**
   The mux sees only an opaque `(name, action, payload)` — it cannot know which host an
   egress-routed capability will reach. So `SsrfPolicy` pairs a host allowlist with a
   `TargetExtractor` (`Fn(&payload) -> Result<Option<Target>, String>`): the def author declares
   how to pull the outbound `(host, port)` out of the payload, and the framework then applies the
   same allowlist + private-IP block `http` applies — pre-connect, inside the mux. `Ok(None)`
   means the action has no outbound target (allowed); `Err` denies fail-closed. No standard
   capability is script-controlled-through-the-mux (`http`/`s3` are in-engine), so this powers the
   extension point; the standard six are all `OperatorSupplied` (no SSRF restriction — the
   `db`/`mail` trust model, connecting to whatever host operator config names).

## Residual risk: cross-capability confused deputy

The per-capability trust model is individually correct and jointly bypassable: a script can read a
secret through an `OperatorSupplied` capability (`db`) and exfiltrate it through a
`ScriptControlled` one (`http`), because all capabilities share one JS heap. No type prevents
this; it is the same trust the operator already extends by injecting the credential. Documented as
a known residual, not a solvable framework property — cross-capability taint tracking would be a
separate, much larger proposal.

## Extending: a custom box

A custom binary composes exactly the capabilities it wants and binds their backends in-process,
without forking core. A `CapabilityDef` is `new(name, js_wrapper, dts_fragment, trust)`, optionally
`.with_backend(egress)`:

```rust
use runlet_core::{CapabilityDef, LogicHost, Trust};

// A capability whose backend runs in-process (e.g. a fabric_backends *Backend, which already
// implements runlet_wire::Egress), plus the stock preset for everything else via the sidecar.
let my_pg = CapabilityDef::new(
    "db",
    include_str!("js/db.js"),      // routes through io.call('db', <action>, payload)
    include_str!("js/db.d.ts"),    // travels with the capability (D11)
    Trust::OperatorSupplied,       // targets from operator config → no SSRF guard
).with_backend(Arc::new(my_in_process_db_backend));

let host = LogicHost::builder(pool, script_registry, settings)
    .capability(my_pg)                       // served in-process
    .capabilities(runlet_caps::preset()       // db is overridden above by name; rest…
        .into_iter().filter(|d| d.name() != "db"))
    .fallback_egress(sidecar)                // …fall through to the fabricd sidecar
    .build()?;                                // duplicate names error here (D1)
```

A `ScriptControlled` capability instead declares `Trust::ScriptControlled(SsrfPolicy::new(allowlist,
extractor))`, and the mux applies the SSRF guard pre-connect. Per-capability metrics surface under
`meta.io.<name>`; regenerate `container/types.d.ts` with `runlet_core::generate_types_dts` (a golden
test guards drift). The stock `runlet` binary is itself just this composition over the `runlet-caps`
preset with the sidecar as the per-request fallback.
