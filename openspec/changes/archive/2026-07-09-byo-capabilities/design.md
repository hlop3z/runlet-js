## Context

The capability pattern is already a string-in/string-out FFI over one primitive: `io.call(name,
action, payload)` (`runlet-core/js/io.js`). Every shipped driver wrapper (`db.js`, …) is just
`io.channel('<kind>')` plus a few one-liners. So "ship a framework, not a service" is mostly
*deletion*: remove the sugar preset (`runlet-caps`), flatten the now-redundant per-kind wire/config
shape, drop the `mongo`/`mongocrypt` tail, and document the extension paths that the library already
supports.

This change also resolves a thread from the same discussion — QUIC vs HTTP for egress. The answer:
they do different jobs. **QUIC (+UDS) is the box↔broker link; HTTP is a capability for reaching
services (including local ones).** Not competitors.

## Goals / Non-Goals

**Goals:**
- Ship exactly three in-engine primitives: `http`, `s3`, `io`.
- Make the three-path extension model (raw http / in-process cap / io+broker) a first-class,
  documented product surface.
- Shrink the box's native + supply-chain surface to the single `aws-lc-rs`/rustls stack (drop mongo).
- Keep the box credential-free and kind-blind (Model 1).

**Non-Goals:**
- Removing the `CapabilityDef` mechanism — it is the whole extension point; only the *shipped preset*
  goes.
- Implementing the reference broker's drivers here — that stays in the sibling repo.
- A statement/target blocklist (unchanged stance).

## The extension spectrum (the product)

```
  LESS infra / LESS isolation  ─────────────────▶  MORE infra / MORE isolation
  the author holds the creds                        the box holds nothing

 (a) http → localhost      (b) in-process cap        (c) io → broker (Model 1)
     your local service        your own binary +          box forwards logical names
     plain http, allowlist     a Rust Cap + Egress        over uds/quic to a broker
     no wire protocol          (your driver/creds)        that holds all creds
        uses `http`               uses the LIBRARY            uses `io` + reference broker
```

Built-ins map one-to-one: `http`→(a), library→(b), `io`→(c); `s3` is an orthogonal signing util.
`io` (c) has two operator-chosen resolutions: a **broker** (uds/quic; box holds nothing remote) or,
for a co-located service declared in the global config, **box-direct-local** over http — logical
naming for a local service without a broker or Rust (D8).

## Decisions

### D1 — Three in-engine built-ins; no shipped driver-cap defs
Ship `http`, `s3`, `io`. The six driver wrappers (`runlet-caps`) are deleted. **Why:** they are sugar
over `io.call`, and shipping them forces the whole driver/audit tail into the product. The
`CapabilityDef` mechanism stays so users reproduce any of them in their own space.

### D2 — Model 1 broker for `io` (box knows only the broker)
`io.call(name, …)` forwards a **logical name** to a broker over **uds** (local) or **quic** (remote);
the broker resolves name → kind → endpoint → creds. The box holds **no** *remote* backend endpoint or
credential. *Rejected:* Model 2 (box dispatches per-transport, holding *remote* internal endpoints and
tokens) — it dissolves the "box holds nothing remote" invariant. The one bounded exception is the
operator-declared, loopback-only box-direct binding (see **D8**), which keeps the invariant intact:
co-located endpoints only, never remote credentials.

### D3 — Flatten `config.io` + `WireInit`; "kind" is operator-side
`config.io` becomes `["orders","cache"]` (a plain allowlist); `WireInit` carries `resources:
Vec<String>`. The resource *kind* stops being a JS identity and becomes a field in the operator
resource entry. **Why:** with no shipped per-kind wrappers the box has no reason to know kinds; the
flat shape is smaller and matches the primitive. *Cost:* a breaking `runlet-wire` change, coordinated
with the reference broker.

### D4 — Drop `mongo` (+ `mongocrypt`)
Remove the mongo capability + driver entirely. **Why:** `mongodb`+`mongocrypt` (C) is the largest
single line in the `cargo vet` / second-crypto-stack tail this repo fights; dropping it shrinks the
surface most. *Breaking* capability removal.

### D5 — Demote the broker to an optional reference image
`fabric`/`fabricd` stops being "shipped core" and becomes a `docker run`-able reference broker for the
Model-1 path. **Why:** the batteries move from inside the box to beside it, matching "framework, not
service." The solo "I just want Postgres" user runs the reference image or writes a cap (honest cost,
named in the proposal).

### D6 — `http` gains a targeted local host:port allowlist
An entry in `http.allowed_hosts` (e.g. `localhost:8000`) **bypasses the private-IP block** for that
named host. **Why:** reaching a co-located cap service (path a) is now a first-class production
pattern; it must not require the blanket `debug` relax (which opens SSRF to the whole internal
network). `debug` stays the dangerous "relax everything" dev knob it is today; the allowlist is the
precise, safe path.

### D7 — QUIC/UDS stay the *broker* link; box-direct-local uses plain HTTP
The box↔**broker** link is uds (local) or quic (remote) — that settles the QUIC-vs-HTTP question for
the broker path. Separately, `io`'s **box-direct-local** resolution (D8) reaches a co-located service
over plain HTTP; this is a *resolution mode*, not a broker transport. So `io` has two operator-chosen
resolutions — broker (uds/quic) and box-direct-local (http, loopback) — while the raw public/local
`http` cap remains its own script-controlled capability. Split by *who controls the target*: `http` =
script, `io` = operator.

### D8 — Logical local egress: allowed, box-direct, via global operator config
`io.call(name, …)` MAY resolve **box-direct** (no broker) to a local endpoint when — and only when —
the operator declares that binding in the **global config** (never per-request, never
script-influenced). This gives "logical local service without a broker or Rust" a first-class path: a
co-located service (e.g. `localhost:8000`) is addressed by a logical name; the box POSTs
`{action, payload}` to the configured endpoint using its existing `http` client. **Constraint (rail):**
box-direct targets are restricted to **loopback/private (co-located)** addresses; a *remote* logical
target must go through a broker (path c). This **tightens** — rather than abandons — Model 1's
invariant to: *the box holds no remote endpoint or credential; it may hold operator-declared,
co-located loopback endpoints.* The script only ever sees a logical name; the target is never
script-controlled, so there is no SSRF surface (operator-supplied trust model).

**Resolution order for `io.call(name, …)`:** name must be in the request's `config.io` allowlist; then
if `name` is in the **global local map** ⇒ box-direct to that loopback endpoint; else ⇒ forward to the
broker (uds/quic). Both paths run through the same mux invariants (allowlist, `meta.io.<name>`
metering, deadline, fail-closed). *(Note: transactions across calls need session affinity; a
box-direct HTTP target must support it server-side, same caveat as any stateless transport.)*

### D9 — One call envelope for broker and box-direct; many local services
A box-direct call SHALL carry the **identical** `{action, payload}` envelope a broker receives in
`WireCall` (the box-direct HTTP body is that JSON; the name is the binding key, not part of the body).
**Why:** the logical name becomes a stable indirection — a service can be *promoted* box-direct →
broker (or moved between localhost ports) with **zero script change**, and a local service is just a
one-name mini-broker over plain HTTP. The global local map holds **many** named bindings, e.g.
`api1 → http://localhost:8080`, `api2 → http://localhost:9000`; each is addressed by its own logical
name through the same `io.call`. This is what makes box-direct and broker interchangeable rather than
two divergent contracts.

## Risks / Trade-offs

- **Ergonomic regression** for the solo "just want a DB" user (loses free `db` sugar). Mitigation: the
  reference broker image + a documented one-file `CapabilityDef` recipe.
- **Cross-repo wire break** (`WireInit` reshape). Mitigation: coordinate the reference broker PR; this
  is a deliberate, one-time break, not additive.
- **Docs churn** — six beginner guides collapse into one. Mitigation: the collapsed guide is the
  canonical "how to extend" doc, which the framework framing needs anyway.

## Open Questions

- **D8 loopback rail** — is the loopback/private-only restriction on box-direct-local targets firm, or
  may an operator point a box-direct `io` binding at any host in global config? Leaning firm
  (loopback-only) to keep the "no remote endpoint/credential" invariant crisp; remote ⇒ broker.
- Do we keep a `db`-shaped `CapabilityDef` as a *reference example* in-repo (a template users copy),
  or purely in docs? Leaning: one reference example in a `examples/` crate, not `runlet-caps`.
- Reference-broker packaging: one image with all five drivers, or per-driver images?

## Build-vs-Adopt Decisions

Recorded by the `/opsx:decide` gate. This change is net **subtraction** — the retained machinery
(`io.call`, `CapabilityMux`, `http`/`s3`, the uds/quic transport, `serde`) is already in-tree, so the
gate reduces to reusing it rather than building anew. Two genuinely-new surfaces (box-direct-local
egress + the `http` allowlist bypass) carry a build-vs-adopt question; both resolved to reuse.

### Decision: Local-target validation — Extend the in-tree `http` SSRF guard

- **Status**: approved
- **Why**: The existing script-facing `http` guard already does private-IP classification, resolved-IP
  checks, and redirect re-validation on **stable** `std::net` (`is_loopback`, `Ipv4Addr::is_private`);
  the box-direct target is **operator-supplied** (global config), not script-controlled, so the
  adversarial DNS-rebinding threat is muted — the need is a loopback-only rail plus the allowlist
  bypass, both a small extension of the one guard we already maintain. One classifier, no new dep.
- **Considered**: *Adopt `agent-fetch`* (Hickory-resolve + pin-to-validated-IP; strong anti-rebinding)
  — overkill for an operator-supplied loopback target and adds a second DNS resolver + a young (Feb
  2026) dep. *Build fresh* — duplicates the existing guard.
- **Isolation**: the `http` capability's SSRF-guard module (`http.rs`), extended with a loopback-only
  check for box-direct bindings and an allowlisted-host bypass of the private-IP block; the box-direct
  resolver calls into it. If box-direct is ever expanded beyond loopback (out of scope per the D8
  rail), `agent-fetch` is the escape hatch to revisit.

### Decision: Box-direct HTTP client — Adopt (reuse) in-tree `reqwest`

- **Status**: approved
- **Why**: The `http` capability already links `reqwest` on the `aws-lc-rs` rustls provider; the
  box-direct POST reuses it, bounded by the existing deadline/mux machinery and fail-closed centrally.
  No new dependency, no second crypto stack (`cargo tree -i ring` stays empty).
- **Considered**: *A minimal/new HTTP client (or `hyper` directly)* — a second egress path with no
  benefit over the client already present.
- **Isolation**: the box-direct resolver behind the `io` mux path; the reqwest client is shared with
  the `http` capability, not a new instance.

### Note: Wire envelope reshape (`WireInit.resources`) — Rent `serde`

Not a build-vs-adopt concern: the flat-name reshape is `serde` (already adopted). Cross-repo
compatibility is a *coordination* matter (the sibling reference broker changes in lockstep), not a
tooling choice — handled in the migration plan, not here.
