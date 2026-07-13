## Context

`box-direct-actor-header` forwards the acting subject to a co-located `io` target via the `X-Runlet-Actor`
header. Box-direct targets are pinned to loopback/private by the `check_local_resources` boot guard, so a
**remote** consumer can never be box-direct — it is reached through a broker (`BrokerEgress`) over UDS or
QUIC, where per-request identity rides the session handshake `WireInit`.

`WireInit` today carries `resources`, `timeout_ms`, `tenant`, and `token` (`runlet-wire/src/wire.rs:188`) —
the tenant but not the actor. `TrustedIdentity.user` is already computed at all three broker-session build
sites (it is passed to the box-direct header), so the actor is in hand; it is simply not put on the
handshake. The broker session is opened per request (`connect_session` per `/execute` / per batch item), so
one session = one actor — the same per-request altitude as the tenant.

The asymmetry this closes: with actor on box-direct only, the tenant-egress "move a service between
box-direct and broker freely" property holds for the tenant (carried on both paths) but silently drops the
actor on the broker path. That makes a shipped feature topology-dependent.

## Goals / Non-Goals

**Goals:**
- Carry the trusted acting subject to the broker in `WireInit`, symmetric with the box-direct header and with
  how the tenant already rides both paths.
- Keep it additive and backward-compatible on the wire (an unchanged `fabricd` is unaffected).
- Keep it trusted-only and script-proof, identical trust rules to `WireInit.tenant`.

**Non-Goals:**
- `fabricd`-side consumption (reading `WireInit.actor`, forwarding to a backend) — separate repo, like the
  box-direct consumer.
- Principal kind (crypto-gated; a future separate field).
- Any change to `http`/`s3`, to `WireCall` (the per-call body stays `{name, action, payload}`), or to the
  box-direct path (already done).
- Roles / entitlements / plan on the handshake.

## Decisions

**Decision: put the actor on `WireInit` (session), not `WireCall` (per call).**
- *Why:* a broker session is per-request and single-actor, exactly like the tenant. Session-level placement
  mirrors `tenant`, avoids repeating identity on every call, and keeps identity out of the per-call body that
  carries the untrusted `payload`. `WireCall` stays `{name, action, payload}`.

**Decision: additive optional field, `#[serde(default, skip_serializing_if = "Option::is_none")]`.**
- *Why:* this is the CLAUDE.md-sanctioned additive `runlet-wire` evolution. `skip_serializing_if` means the
  single-tenant/loopback path (subject `None`) serializes a byte-identical handshake to today, and an
  unchanged `fabricd` deserializes an absent field as `None` — no coordinated deploy, no breakage. Mirrors
  `tenant` exactly.
- *Debug:* add `actor` to the hand-written `WireInit` `Debug` impl. Unlike `token` it is **not** a secret
  (it is the same class of trusted-edge data as `tenant`, which is already printed), so print it plainly.

**Decision: source solely from `TrustedIdentity.user`, include only when present.**
- *Why:* identical trust rules to the tenant; the value is read at the handler layer from the trusted
  extractor, never re-derived in the sandbox. `None` ⇒ no actor on the handshake.

**Decision: thread through `wire_init(...)`, mirroring `tenant`.**
- Add `actor: Option<&str>` to `wire_init` (`handler/types.rs`); set `WireInit.actor = actor.map(str::to_owned)`.
- Pass `identity…user` at all three call sites (`mod.rs`, `lifecycle.rs`, `batch_items.rs`) — the same value
  already fed to the box-direct header. Update the `WireInit` literals in tests to construct/assert the field.

## Risks / Trade-offs

- **Producer ahead of a `fabricd` consumer** → bounded to a bare subject; additive/optional so an unchanged
  broker is untouched. Justified now: a remote/QUIC consumer is confirmed in scope, and without this the
  box-direct actor feature is topology-dependent.
- **Actor crosses the network to a remote broker over QUIC** → same authenticated channel (pinned cert +
  `token`) and same trusted-edge provenance as the `tenant` that already crosses it; no new exposure.
- **Contract drift with `fabricd`** → additive/optional by construction; `runlet-wire` is this repo's owned
  contract and the field is `#[serde(default)]`, so old and new brokers interoperate.

## Migration Plan

Additive and backward-compatible. No config change. Single-tenant deployments send no actor (`None`).
An unchanged `fabricd` ignores the absent/unknown field; a future `fabricd` opts in by reading
`WireInit.actor`. Rollback is a code revert with no data or config migration.

## Open Questions

None. (If a consumer later requires principal kind on the broker path, that is a new change: a nexus
bare-header emission plus a separate `WireInit` field — never in-box signed-contract verification.)
