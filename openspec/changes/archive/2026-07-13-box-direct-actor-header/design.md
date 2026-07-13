## Context

Egress in the box has three paths: broker (`BrokerEgress`), box-direct (`local_io::BoxEgress`), and the
in-engine `http`/`s3` built-ins. The `box-direct-tenant-header` change taught the box-direct path to carry
the acting **tenant** (`X-Runlet-Tenant`, sourced from `TrustedIdentity.tenant`, emitted only when present,
body unchanged — `local_io.rs:129` `build_request`). This change adds the second, symmetric axis: the
acting **subject** (`X-Runlet-Actor`, sourced from `TrustedIdentity.user`).

`TrustedIdentity.user` is already resolved on every request from the trusted `x-user-id` header
(`identity.rs:36`, `:68`) and used for telemetry (`telemetry.rs:166`) — it is in scope at all three
box-direct build sites but simply not threaded into `BoxEgress`, exactly as the tenant was before its
change.

The concrete driver is using an event-log-style service as a box-direct `io` target. A verified read of
the sibling event-logs repo established the shape the design must respect:

- Its write path today consumes **tenant + stream only** (`core.Identity { Tenant, Stream }`); there is
  **no actor/subject/kind field** yet, and its own integration doc defers actor for the same reason the
  runlet handoff note did.
- Its box-direct adapter is planned to read **tenant from the `X-Runlet-Tenant` header** and **stream from
  the decoded `payload`**. That split is the load-bearing insight: a *stream/routing key* can come from the
  untrusted payload because it is not a trust assertion; an *actor* cannot — a who-did-what sourced from
  script-controlled payload is forgeable and worthless for audit. So the only correct channel for a trusted
  actor is an out-of-band header, precisely mirroring the tenant.

Constraint that shapes the design (unchanged from the tenant change): the D9 invariant — a box-direct call
carries the identical `{action, payload}` envelope a broker receives, so a service moves between box-direct
and broker with no change to the calling script or wire body (`tenant-egress` spec; `local_io.rs:11`).

## Goals / Non-Goals

**Goals:**
- Convey the request's trusted acting subject to the box-direct loopback endpoint so a consumer can build a
  who-did-what audit trail.
- Keep it trusted-only and script-proof, on the same terms as `X-Runlet-Tenant` / `WireInit.tenant`.
- Preserve the D9 identical-`{action, payload}`-body invariant.

**Non-Goals:**
- Forwarding principal **kind** (user/apikey/service) — crypto-gated; see the decision below.
- Any change to `http` / `s3` (they target non-privatized endpoints — no identity).
- Any change to the broker path or the `runlet-wire` contract (adding actor to `WireInit` would change the
  cross-repo contract; not in scope, and the broker's credential-holder does not need the subject to resolve).
- Forwarding roles, entitlements, plan, or member-type.
- Adding the consuming field on the event-logs side (separate repo, separate change).

## Decisions

**Decision: forward the actor now, ahead of a consumer.**
- *Why:* the value is already in hand (zero new machinery, zero crypto), the trusted channel is the only
  correct one for an actor, and the user chose to have it on the wire so a consumer field can light up with
  no box redeploy. The cost is one additive, namespaced header on a loopback POST that current consumers
  (including event-logs today) simply ignore.
- *Honest trade-off:* this is a trusted producer ahead of its consumer — a mild YAGNI. It is bounded by
  shipping the *bare subject* only (no format guess to regret) and by the separate-header rule for kind
  (below), so nothing here has to break when a real consumer defines its needs.

**Decision: convey the subject as an out-of-band header (`X-Runlet-Actor`), bare value.**
- *Why:* identical rationale to `X-Runlet-Tenant` — keeps the body exactly `{action, payload}` (D9), keeps
  a trust assertion out of the same JSON object as the untrusted `payload`, and matches the `x-runlet-*`
  trusted-header family. The value is the bare subject string (`TrustedIdentity.user`), not `{kind}:{subject}`.
- *Alternative (rejected):* encode `{kind}:{subject}`. Rejected because kind is not available without crypto
  (below), and baking a compound format before a consumer defines it is the format-guess trap. Bare subject
  is the stable, minimal form.

**Decision: do NOT forward principal kind; if ever needed, a separate header.**
- *Why:* kind (user/apikey/service) rides only inside the signed `x-identity-contract`. Extracting it in the
  box means verifying a signed assertion on the `/execute` hot path — re-opening the settled "no crypto in
  the box" invariant (`multitenant-trust.md`, the same reason in-box JWT verification was rejected for the N5
  scope tripwire). A bare `x-user-kind` header from the edge would be the cheaper source, but that is a nexus
  ask, not a box change. Either way it is a *future, separate* header so `X-Runlet-Actor` stays stably equal
  to the subject.

**Decision: source solely from `TrustedIdentity.user`, emit only when present.**
- *Why:* identical trust rules to the tenant. On the single-tenant / non-trusted path the subject is `None`,
  so no header is added and the request is byte-for-byte unchanged. Read at the handler layer from the
  trusted extractor, before the blocking closure, never re-derived inside the sandbox.

**Decision: thread the subject through the existing params struct, mirroring the tenant.**
- Add `actor: Option<String>` to `ExecuteBlocking` (`handler/mod.rs`), populated at each build site from the
  same `identity.and_then(|id| id.user.as_deref().map(str::to_owned))` pattern used for `tenant`.
- Add an `actor: Option<String>` param to `BoxEgress::new` (`local_io.rs`), stored on the struct; attach
  `.header("x-runlet-actor", actor)` in `build_request` when `Some`. `BoxEgress::new` already carries an
  `#[expect(clippy::too_many_arguments, …)]`; extend the reason.
- *Why:* the value already exists at every site; pure field-threading, no new lookups/config/lint surface.

## Risks / Trade-offs

- **Producer without a consumer** → bounded to a bare subject (no format regret) + separate-header rule for
  kind; additive/namespaced header that consumers ignore harmlessly. Documented as a deliberate call.
- **A loopback service that trusts inbound headers blindly** → target is loopback/private, operator-declared,
  boot-guard-pinned; header is `x-runlet-*`-namespaced and additive.
- **Scope creep toward forwarding roles/entitlements/plan/kind** → explicitly non-goals; only the subject is
  forwarded, keeping box-direct parity minimal and mirroring what the tenant change established.

## Migration Plan

Additive and backward-compatible. No wire-contract or config change. Existing single-tenant deployments send
no header (subject `None`). A multitenant operator opts in by having their loopback service read
`X-Runlet-Actor`. Rollback is a code revert with no data or config migration.

## Open Questions

None. (If a consumer later requires principal kind, that is a new change: a nexus bare-header emission plus a
second box-direct header — never in-box signed-contract verification.)
