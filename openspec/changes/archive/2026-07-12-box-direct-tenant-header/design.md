## Context

Egress in the box has three paths: broker (`BrokerEgress`), box-direct (`local_io::BoxEgress`), and
the in-engine `http`/`s3` built-ins. The broker path already carries the acting tenant to the
credential-holding daemon at session open — `wire_init(broker_names, timeout, tenant)` populates
`WireInit.tenant`, sourced from `TrustedIdentity.tenant` (`lifecycle.rs:116`, `batch_items.rs:399`).

The box-direct path is a per-call HTTP POST to a co-located loopback service the operator declared in
the global `local_resources` map. It has **no session** — each `io.call` is one `POST` of a
`LocalCallEnvelope { action, payload }` (`local_io.rs:51`, `call_local` at `local_io.rs:110`). That
envelope carries no identity, so a multitenant loopback service cannot scope its data by tenant. The
tenant is already in scope at both `BoxEgress` build sites but is simply not threaded in.

Constraint that shapes the whole design: the D9 invariant — *"a box-direct call carries the identical
`{action, payload}` envelope a broker receives, so a service moves between box-direct and broker with
no change to the calling script or wire body"* (`tenant-egress` spec; `local_io.rs:11`).

## Goals / Non-Goals

**Goals:**
- Convey the request's trusted tenant to the box-direct loopback endpoint so it can scope by tenant.
- Keep it trusted-only and script-proof, on the same terms as `WireInit.tenant`.
- Preserve the D9 identical-`{action, payload}`-body invariant.

**Non-Goals:**
- Any change to `http` / `s3` (they target non-privatized endpoints — no identity).
- Any change to the broker path or the `runlet-wire` contract.
- Forwarding user id, roles, plan, or any identity field other than the tenant.
- Per-call identity on the broker path (it stays session-scoped via `WireInit`).

## Decisions

**Decision: convey the tenant as an out-of-band HTTP header (`X-Runlet-Tenant`), not a body field.**
- *Why:* a header keeps the request **body** exactly `{action, payload}`, preserving D9 — a service
  still moves between box-direct and broker with an unchanged wire body. Identity is transport/session
  metadata, mirroring how the broker keeps the tenant in `WireInit` (session) rather than `WireCall`
  (the per-call body). It also keeps the trusted tenant out of the same JSON object that carries the
  untrusted script `payload`.
- *Alternative (rejected):* add `tenant` to `LocalCallEnvelope`. This breaks D9 (box-direct and broker
  bodies diverge) and co-locates trusted identity with untrusted payload in one object.
- *Header name:* `X-Runlet-Tenant`, matching the existing `x-runlet-*` trusted-header family
  (`x-runlet-capture`, `x-runlet-mode`, `x-runlet-log-level`).

**Decision: source the value solely from `TrustedIdentity.tenant`, emit the header only when present.**
- *Why:* identical trust rules to `WireInit.tenant`. On the single-tenant / non-trusted path the tenant
  is `None`, so no header is added and the request is byte-for-byte unchanged (no behavior change for
  existing single-tenant deployments).
- The script and request body can never influence it — the value is read at the handler layer from the
  trusted-identity extractor, before the blocking closure, never re-derived inside the sandbox.

**Decision: thread the tenant through the existing params struct, no new plumbing.**
- Add `tenant: Option<String>` to `ExecuteBlocking` (`handler/mod.rs:333`), populated at both build
  sites from the same `identity.and_then(|t| t.tenant.as_deref())` already computed for `wire_init`.
- Add a `tenant: Option<String>` param to `BoxEgress::new` (`local_io.rs:80`), stored on the struct.
- In `call_local` (`local_io.rs:110`), attach `.header("x-runlet-tenant", tenant)` when `Some`.
- *Why:* the value already exists at both sites; this is a field-threading change, no new lookups, no
  new config, no lint-surface risk (mirror the existing `http.rs`/`local_io.rs` idioms).

## Risks / Trade-offs

- **A loopback service that echoes or trusts inbound headers blindly could be confused by the new
  header** → the target is loopback/private and operator-declared (boot-guard pinned); the header is
  namespaced (`x-runlet-*`) and additive; services that ignore it are unaffected.
- **Header vs. body asymmetry with the broker could surprise a reader** → documented here and in the
  spec: the tenant is session/transport metadata in both paths (`WireInit` for the broker, a per-call
  header for the sessionless box-direct path); the `{action, payload}` body stays identical.
- **Scope creep toward forwarding more identity fields** → explicitly a non-goal; only the tenant is
  forwarded, keeping parity with what the broker path already gets.

## Migration Plan

Additive and backward-compatible. No wire-contract or config change. Existing single-tenant
deployments send no header (tenant `None`) and are unaffected. A multitenant operator opts in simply by
having their loopback service read `X-Runlet-Tenant`. Rollback is a code revert with no data or config
migration.

## Open Questions

None.
