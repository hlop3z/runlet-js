## Why

`runlet` is already a library (`runlet-core`) with a first-class capability-composition seam
(`CapabilityDef` + `LogicHost::builder`). Today it *also* ships a batteries-included preset — six
driver-backed capability wrappers (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) in `runlet-caps` — plus a
per-kind `config.io`/`WireInit` shape and a mongo driver whose `mongocrypt` (C) tail is the single
worst line in this repo's supply-chain audit surface. That preset is *sugar*: every wrapper is a thin
`io.channel('<kind>')` over the one primitive that already exists — `io.call(name, action, payload)`
(`runlet-core/js/io.js`).

We want to lean all the way into "runlet is a framework, not a service": ship the **three in-engine
primitives** and let users add everything else. This is subtraction that reveals a primitive already
present, not new machinery.

- **Ship exactly three built-ins:** `http` (script-controlled URL, SSRF-guarded), `s3` (pure SigV4
  signing), and `io` (operator-named logical egress to a broker). Delete the six driver-cap wrappers;
  the `CapabilityDef` mechanism stays as the user extension point.
- **Make the extension spectrum first-class and documented** — paths by trust/infra need:
  (a) raw `http` to a service (incl. an allowlisted `localhost:8000`); (b) a Rust cap compiled into
  your own binary via the library (your driver, your creds, your process); (c) `io` → a broker that
  holds all creds (box holds nothing — the multitenant path). Path (c) also has a **broker-free
  variant** — `io` resolving **box-direct** to a co-located local service the operator declares in the
  **global config** (logical naming for a `localhost:8000` service without a broker or Rust; D8).
- **Flatten the wire/config** now that "kind" is an operator concern: `config.io` becomes a plain
  allowlist of logical names; `WireInit` carries `resources: Vec<String>` instead of six per-kind
  slots.
- **Drop `mongo`** (and `mongocrypt`) entirely.
- **Demote the broker** (`fabric`/`fabricd`) from shipped-core to an **optional reference image** —
  the batteries move from *inside the box* to *beside it*.

## Capabilities

### Modified Capabilities
- `capability-registry`: the built-in capability set is fixed at three in-engine primitives
  (`http`, `s3`, `io`); driver-backed capabilities are **user-composed** via `CapabilityDef`, not
  shipped. Adds the documented three-path extension model.
- `tenant-egress`: egress addresses a **flat list of logical resource names** (`config.io`), forwarded
  in `WireInit` as `resources: Vec<String>`; the resource *kind* + transport (uds/quic) is resolved
  entirely operator-side by the broker (Model 1: the box holds no remote endpoint/cred). A name may
  instead be bound **box-direct** to a co-located loopback endpoint via the operator's global config
  (D8) — logical, broker-free, and the one bounded exception to Model 1 (co-located only).
- `http`: gains a **targeted local host:port allowlist** that bypasses the private-IP block for
  explicitly-named hosts, so reaching a co-located cap service (`localhost:8000`) is production-safe
  without the blanket `debug` SSRF relax.

### Removed Capabilities
- `db`, `mongo`, `mail`, `redis`, `amq`, `auth`: removed as **shipped** capabilities (the JS/TS
  wrappers, `.d.ts` fragments, and action-token lists leave the box). Their behavior is reproducible
  by a user `CapabilityDef` over `io.call`, and the reference broker still implements the drivers.

## Impact

- **`crates/runlet-caps` — deleted** (all six defs + `js/*.js` + `*.d.ts` + `actions::*` + fixtures).
- **`crates/runlet-core`** — keeps `io.call`/`__io`, the `CapabilityMux`, the `CapabilityDef`
  mechanism, and the `http`/`s3` in-engine caps; the `http` SSRF policy gains the local allowlist.
- **`crates/runlet-wire`** — `WireInit` reshapes from six per-kind `Option<String>` slots to
  `resources: Vec<String>`. **Cross-repo breaking wire change**, coordinated with the reference broker.
- **`crates/runlet`** — `RequestIo` (per-kind) collapses to a flat name list; the sidecar forwards the
  name list; no kind logic in the box. Adds an optional **global local-resource map** (name → loopback
  endpoint) for the box-direct `io` resolution (D8), consulted before falling through to the broker.
- **`docs/`** — the six beginner capability guides (`03`,`04`,`07`,`08`,`10`,`11`,`12`) collapse into
  one "build your own capability over `io.call`" guide; `docs/design/resource-egress.md` keeps the
  least-privilege section (carried from `resource-privilege-guard`).
- **`fabric`/`fabricd` (sibling repo)** — demoted to the optional reference broker image; still holds
  the drivers + creds for the Model-1 path. Removes mongo.
- **Supersedes `resource-privilege-guard`** — that change's box-side code is already reverted to zero;
  its operator docs + hardened-role recipes fold into this change. Park, then archive on sync.
- **Operators (BREAKING):** a deployment relying on the shipped `db`/… wrappers must either run the
  reference broker, write a `CapabilityDef`, or reach a local service over `http`. Scripts calling
  `db.query(...)` must move to `io.call("<name>", "query", ...)` or a user-supplied wrapper.
