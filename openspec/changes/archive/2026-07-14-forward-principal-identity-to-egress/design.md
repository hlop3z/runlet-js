## Context

The archived `conform-trusted-identity-to-signed-contract` change made the box **verify** nexus's signed
`x-identity-contract` and **capture** `principal_kind` + `on_behalf_of` onto `TrustedIdentity`
(`crates/runlet/src/identity.rs`). But the two egress build sites forward only tenant + actor:

- Broker: `wire_init(broker_names, timeout, tenant.as_deref(), identity.user.as_deref())`
  (`handler/mod.rs`); `WireInit` (`crates/runlet-wire/src/wire.rs`) has only `tenant` + `actor`.
- Box-direct: `BoxEgress.actor = identity.user`; headers are `x-runlet-tenant` + `x-runlet-actor`
  (`local_io.rs`).

`actor` is `identity.user` on both paths, which for an `apikey` principal is the **key id**, not the human.
So the human (`on_behalf_of`) and the `principal_kind` are dropped at the re-vouching boundary. This change
threads both fields through. It is greenfield (no live consumers), so the wire may change freely.

## Goals / Non-Goals

**Goals:**

- Forward `principal_kind` and `on_behalf_of` on **both** egress transports, alongside (not replacing)
  `actor`, emitted only when a verified contract populated them.
- Keep the box-direct `{action, payload}` body byte-identical (D9); keep the plain / single-tenant
  handshake and POST unchanged.

**Non-Goals:**

- No **gating** on `principal_kind` (admit a `service` writer vs a human) — separate open question.
- No `fabricd` change and no event-logs consumption — the sibling/other repos own those.
- `actor` semantics unchanged (stays the bare acting subject).

## Decisions

> **Build-vs-adopt gate:** ran, no concern found. This change adds two `Option<String>` fields to an
> existing struct + two headers, threading already-captured data — no new dependency, no crypto, no
> tool selection. The only fork is the wire *shape* (a design choice, below), confirmed at the gate.

### Decision: Wire representation — Build (structured fields, hand-written)

- **Status**: approved
- **Why**: independent `WireInit` fields / `x-runlet-*` headers keep `actor` stably equal to the subject
  and give `on_behalf_of` a real slot; the overloaded `{kind}:{subject}` string is lossy (no place for the
  human) and forces downstream parsing.
- **Considered**: overloaded actor string (event-logs' sketched form) — dropped as lossy/ambiguous.
- **Isolation**: the two fields live on `WireInit` (`runlet-wire`) and as `x-runlet-*` header consts in
  `local_io.rs`; the rest of the box only reads them off `TrustedIdentity`.

### D1 — Structured fields, not an overloaded actor string (settled)

Add `principal_kind` and `on_behalf_of` as **independent** `WireInit` fields / `x-runlet-*` headers rather
than encoding `{kind}:{subject}` into `actor`. **Why:** each field is independently optional, `actor` stays
stably equal to the subject (no re-parse downstream), and there is a clean slot for `on_behalf_of`. The
overloaded-string form (event-logs' planned `X-Runlet-Actor: {kind}:{subject}`) is lossy — no place for the
human — and forces string parsing on the consumer. `on_behalf_of` is meaningful only for `apikey`, so both
fields are `Option`, absent otherwise.

### D2 — Emit only from a verified contract (settled)

Both fields are populated only when the signed-contract sub-mode verified the request (they are `None` on
the plain trusted-header path and single-tenant). This keeps the plain/loopback wire unchanged and means a
present `principal_kind` on the wire is always a *verified* fact, never a bare-header assertion. Mirrors how
`tenant`/`actor` are already gated on presence.

### D3 — `skip_serializing_if = "Option::is_none"` (settled)

Both `WireInit` fields skip serialization when `None`, so the single-tenant handshake stays byte-minimal
and an unchanged broker simply ignores unknown/absent fields. Greenfield doesn't *require* compatibility,
but this is the same additive shape `tenant`/`actor` already use — consistency over a gratuitous break.

### D4 — Box-direct headers mirror the existing pattern (settled)

Two new header consts `X-Runlet-Principal-Kind` / `X-Runlet-On-Behalf-Of` in `local_io.rs`, attached with
the same `if let Some(..)` guard as `x-runlet-tenant`/`x-runlet-actor`, body untouched. `http`/`s3` carry
none (unchanged).

## Risks / Trade-offs

- **[Header/field name drift vs a future consumer]** → names are chosen to mirror the existing
  `x-runlet-*` / `WireInit` vocabulary; documented in `tenant-egress` spec + `multitenant-trust.md`.
- **[A consumer treats a present `principal_kind` as authorization]** → out of scope here; forwarding is
  attribution only. Gating remains a separate, explicitly-deferred change; the spec frames these as
  attribution fields.
- **[`runlet-wire` is the cross-repo contract]** → additive `Option` fields; an unchanged `fabricd`
  compiles and ignores them. Coordinating `fabricd` to *use* them is a separate, later change.

## Migration Plan

Greenfield — no live traffic. Ship together: the `runlet-wire` field addition and the box-side threading in
one change (the box is the only producer). No rollout ordering needed; a broker that doesn't read the fields
is unaffected.

## Open Questions

- Gating on `principal_kind` (service-writer admission) — deferred.
- Whether `fabricd`/event-logs should key audit on `on_behalf_of` — the consumer repos' call.
