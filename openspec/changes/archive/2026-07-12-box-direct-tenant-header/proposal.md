## Why

The box already forwards the request's trusted tenant identity to the **broker** at session open
(`WireInit.tenant`), so a broker resolves resources scoped to the acting workspace. But the
**box-direct** path — a logical name the operator bound to a co-located loopback service in
`local_resources` — POSTs only `{action, payload}` with no identity. A box-direct service is a
*privatized* resource (loopback/private, boot-guard pinned), so in a multitenant deployment it needs
the acting tenant to scope its own data, exactly as the broker does. Today it cannot: the two egress
paths are asymmetric on identity.

## What Changes

- The box-direct POST SHALL carry the request's trusted tenant identity as an out-of-band HTTP header
  (`X-Runlet-Tenant`) when — and only when — a trusted tenant identity is present. The
  `{action, payload}` request **body** is unchanged, preserving the invariant that a service moves
  between box-direct and broker resolution with no change to the wire body.
- The header is sourced **only** from the trusted-identity extractor (`TrustedIdentity.tenant`), never
  from anything the executing script can influence. On the single-tenant / non-trusted path (no
  trusted tenant), **no** header is added and behavior is byte-for-byte unchanged.
- Scope is **box-direct only**. The `http` and `s3` built-ins are explicitly excluded — they reach
  script-controlled or externally-signed targets, not the operator's privatized resources, so they
  carry no identity. The broker path is unchanged (it already carries the tenant via `WireInit`).

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `tenant-egress`: the "Box-direct local egress binding" requirement gains a companion requirement —
  the box-direct call carries the trusted tenant identity as an out-of-band header, on the same
  trusted-only, script-proof terms as the broker session's `WireInit.tenant`, without altering the
  `{action, payload}` envelope.

## Impact

- **Code (this repo only):** `crates/runlet/src/local_io.rs` (`BoxEgress` stores the tenant;
  `call_local` emits the header), `crates/runlet/src/handler/mod.rs` (`ExecuteBlocking` gains a
  `tenant` field), and its two build sites `crates/runlet/src/handler/lifecycle.rs` +
  `handler/batch_items.rs` (pass the already-computed tenant, reusing the `wire_init` value).
- **No wire-contract change:** `runlet-wire` is untouched; box-direct is plain HTTP to a loopback
  service, not the `WireCall` framing.
- **Downstream:** an operator's co-located loopback service MAY now read `X-Runlet-Tenant` to scope
  per tenant; services that ignore it are unaffected (additive header).
- **Out of scope:** `http`, `s3`, and the broker path.
