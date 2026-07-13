## Why

The `box-direct-actor-header` change taught the box to forward the acting subject (`X-Runlet-Actor`) to a
co-located loopback `io` target — but box-direct is **loopback-only** by the boot guard. A **remote** `io`
consumer cannot be box-direct; it is reached through a broker over QUIC, where identity rides the
`WireInit` handshake. `WireInit` carries the trusted **tenant** but **not** the actor. So actor forwarding
is currently *topology-dependent*: it works when the consumer is co-located, and silently drops the actor
the moment the same consumer is reached remotely over QUIC. That breaks the logical-name indirection's
promise that a service moves between box-direct and broker with its identity intact, and it leaves a real
remote/QUIC consumer (confirmed in scope) without the who-did-what signal the box already holds.

## What Changes

- `WireInit` gains an `actor: Option<String>` field, **additive and backward-compatible** (`#[serde(default,
  skip_serializing_if = "Option::is_none")]`), sitting beside `tenant` — same shape, same trust source
  (`TrustedIdentity.user`), same per-request session lifecycle. The box populates it when, and only when, a
  trusted subject is present; `None` on the single-tenant/loopback path (byte-identical handshake to today).
- The `wire_init(...)` helper gains an `actor: Option<&str>` parameter; all three broker-session build sites
  (`handler/mod.rs`, `handler/lifecycle.rs`, `handler/batch_items.rs`) pass the same `identity.user` they
  already have in scope for the box-direct header.
- The `WireInit` manual `Debug` impl prints the new `actor` field (it is not a secret, unlike `token`).
- Result: actor forwarding becomes **topology-independent** — a consumer reached box-direct (`X-Runlet-Actor`
  header) or over a UDS/QUIC broker (`WireInit.actor`) receives the same acting subject, exactly as the
  tenant already does on both paths.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `tenant-egress`: the "Tenant identity carried on the egress session" requirement gains a companion —
  "Actor identity carried on the egress session" — asserting the box includes the trusted acting subject in
  `WireInit`, on the same trusted-only, script-proof, present-only terms as the tenant, so the broker (and
  any backend behind it) can attribute who-did-what regardless of transport.

## Impact

- **Contract (this repo owns it):** `crates/runlet-wire/src/wire.rs` — `WireInit` gains `actor`; the change
  is additive/compatible, so an unchanged `fabricd` deserializes it as absent and is unaffected. Per
  CLAUDE.md this is the sanctioned additive `runlet-wire` evolution, not a coordinated breaking change.
- **Code:** `crates/runlet/src/handler/types.rs` (`wire_init` signature), and the three build sites that call
  it (`mod.rs`, `lifecycle.rs`, `batch_items.rs`) plus the `WireInit` literals in tests.
- **Downstream (separate repo, out of scope here):** `fabricd` MAY read `WireInit.actor` and forward it to a
  backend driver for audit — exactly like the box-direct consumer (event-logs) reads `X-Runlet-Actor`. This
  change only makes the actor *available* on the broker path; who consumes it is downstream.
- **Out of scope:** `http`/`s3`, principal kind (crypto-gated; a future separate field), roles/entitlements/
  plan, and any `fabricd`-side consumption.
