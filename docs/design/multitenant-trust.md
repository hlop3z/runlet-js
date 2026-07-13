# Multitenant trust: `runlet` as a nexus edge backend

`/execute` runs untrusted, caller-supplied JavaScript. This design makes `runlet` safe for
**untrusted multi-tenant** traffic by deploying it as a backend pool (`pool_jsbox`) behind the
first-party **nexus** edge platform (Envoy + tenant-router + identity plane), and keying every
per-tenant boundary — fairness, cache, egress scope, quota — off a **trusted, edge-authorized
tenant identity** that no caller can assert or forge.

See `openspec/changes/runlet-nexus-multitenant-trust/` for the proposal, specs, and decision record.

## The trust flip

`runlet` performs **no** TLS termination, JWT verification, or user authentication — the edge does
(`jwt_authn` + on-demand TLS), and it strips any client-supplied `x-*` header before injecting a
trusted identity. `runlet` consumes that identity from operator-configured trusted headers:

| Purpose            | Default header         | Field                     |
| ------------------ | ---------------------- | ------------------------- |
| acting workspace   | `x-workspace-id`       | `tenant` (the universal key) |
| user (audit)       | `x-user-id`            | `user`                    |
| member roles       | `x-user-roles`         | `roles` (comma-separated) |
| member entitlements| `x-user-entitlements`  | `entitlements` (comma-sep)|
| suspended flag     | `x-user-suspended`     | `suspended` → hard reject |
| anonymous flag     | `x-auth-anonymous`     | `anonymous` → hard reject |
| plan (quota tier)  | `x-tenant-plan`        | `plan`                    |
| acting-org scope   | `x-tenant-scope`       | `scope` → must be `acting` (N5) |

Every name is configurable (`trusted.headers.*`) so a drift between the edge contract and the box is
pinned in one place. The tenant default is `x-workspace-id` — the name the nexus identity sidecar
injects (`x-tenant-id` survives only as a legacy read-fallback *inside* nexus and is never emitted
toward boxes); the pinned contract is the "Downstream header contract" table in
`nexus-upstream-requirements.md`. Trusted mode is **opt-in** (`trusted.enabled`); the default preserves the
pre-change single-principal, loopback behavior.

## The trust invariant (and its safety net)

Because `runlet` trusts `x-*` blindly once enabled, the entire model rests on one invariant:

> **`runlet` is reachable only through the edge.**

Enforced out of band by a k8s **NetworkPolicy** (`deploy/networkpolicy-pool-jsbox.yaml`) restricting
ingress to `pool_jsbox` to the edge namespace/pod-selector. The in-process **boot guard**
(`config.rs::check_trusted_isolation`) is the fail-closed backstop: trusted mode refuses to start on
a non-loopback bind unless the operator asserts `trusted.assert_network_isolation: true` — mirroring
the existing `allow_unauthenticated` guard, because there is no TLS/JWT check to fall back on once
headers are trusted. The existing `access_token` is repurposed as the **edge→box service
credential** (defense in depth with the NetworkPolicy).

Three independent controls must all hold: the NetworkPolicy, the boot guard, and the service
credential. The guard fails closed.

## Tenant is the universal key

The trusted tenant id (the acting workspace, opaque to the box) is the single key for:

- **Tier 5 fairness** (`PartitionLimiter`): the partition key is the trusted tenant id. The
  caller-asserted `X-Partition-Key` header / `partition` body source is **removed** in trusted mode
  (it was a noisy-neighbor evasion + cross-tenant cache-dedup/timing vector). `meta.partition` still
  echoes the resolved value. Intra-workspace fairness is accepted (a shared bucket, like a shared CI
  runner).
- **Bytecode-cache namespace**: identical source from different tenants never shares a cache entry
  (no cross-tenant dedup / compile-timing leak).
- **Egress scope**: the box forwards the trusted tenant id in `WireInit`; `fabricd` resolves logical
  resource names **only within that tenant's binding set** (`tenant` on each `TenantResourceBinding`;
  a cross-tenant name resolves as `NotFound` so existence never leaks). Credentials never cross
  workspaces, enforced where credentials live.
- **Quota**: per-tenant plan-gated usage (below).

`runlet` treats `x-workspace-id` as opaque and already-authorized; it never branches on "user vs org"
and never learns how the acting workspace was chosen (that is nexus upstream requirement **N5** —
see `nexus-upstream-requirements.md`).

### Cross-repo vocabulary and identity axes

The same concept carries a different load-bearing noun in each repo; the values are the same literal
string, propagated verbatim through every hop (renaming any of them would cost more than it buys):

```
nexus Workspace.id  ==  runlet "trusted tenant"  ==  event-logs Tenant
  x-workspace-id  ─▶  (runlet keys on it)  ─▶  X-Runlet-Tenant  ─▶  event-logs Tenant field
```

nexus's `docs/tenancy-and-identity.md` is the canonical model. It separates **three** identity axes;
`runlet`'s relationship to them is deliberately lopsided:

- **Workspace** (*where* — the tenancy boundary, nexus-minted `ws_<uuidv7>`) — this is the "trusted
  tenant"; `runlet` **keys** every per-tenant boundary above on it.
- **Actor** (*who* — the acting principal, a federated subject from `x-user-id`) — `runlet` **carries**
  it for audit (`TrustedIdentity.user`) but reads nothing off it for scoping.
- **Account** (*who owns/pays*, nexus-minted `acct_<uuidv7>`) — one level above workspace; it **never
  reaches** `runlet` and needs no box-side concept.

`runlet` deliberately abstracts nexus's `Workspace` into a generic `tenant` so it stays usable behind
any identity plane, not permanently bound to nexus — the equivalence is written down, not unified.

### Identity on the egress paths

The acting identity — tenant (*where*) and actor (*who*) — rides **both** egress transports on equal
terms, so a logical `io` name can move between a co-located loopback service and a remote broker without
dropping identity. Box-direct carries it as out-of-band `x-runlet-*` headers; the broker carries it in the
`WireInit` session handshake (`tenant` + `actor`). The `{action, payload}` body is identical on both (D9).

A box-direct `io` target (an operator-declared co-located loopback service) receives the acting identity
as out-of-band `x-runlet-*` headers, never in the `{action, payload}` body (D9):

- **`X-Runlet-Tenant`** (*where*) — the trusted tenant; the box-direct analogue of the broker's
  `WireInit.tenant`. Emitted only when a trusted tenant is present.
- **`X-Runlet-Actor`** (*who*) — the trusted acting **subject** (`TrustedIdentity.user`, the bare
  `x-user-id` value), so a consumer can build a who-did-what audit trail. Emitted only when a trusted
  subject is present.

Both are sourced **only** from the trusted-identity extractor. A routing key like an event stream may
ride the untrusted `payload`, but an actor is a *trust assertion* and therefore must be out-of-band —
the payload path is off-limits for it.

Principal **kind** (user/apikey/service) is deliberately **not** forwarded on either path: it lives only
inside the signed `x-identity-contract`, and the box verifies no signed assertions (the "no crypto in the
box" invariant — the same reason in-box JWT verification was rejected for the N5 tripwire below). Should a
consumer ever require kind, it arrives as a *separate* trusted field (a nexus bare-header emission the box
forwards as its own header / `WireInit` field), leaving the actor stably equal to the bare subject. See
`openspec/specs/tenant-egress/spec.md`.

## Acting-org assurance (the N5 tripwire)

Because `x-workspace-id` is opaque, `runlet` cannot tell an *authorized acting org* apart from a user's
*home org* — an edge that has not shipped N5 (or has drifted) would inject the home org and `runlet`
would **silently mis-scope** a multi-org user across all four boundaries above. To close that gap the
edge asserts acting-org authorization per request with a trusted `x-tenant-scope: acting` header, and
`runlet` **enforces it fail-closed**: a tenant-scoped `/execute` whose `scope` is absent or not equal
to `acting` is rejected `403 ACTING_SCOPE_REQUIRED` before any egress session or execution. The gate
sits in `resolve_identity` alongside the anonymous / suspended / tenant-less hard-rejects — one more
trusted-header read at the same altitude.

This is **intrinsic to trusted mode** — no opt-in flag, no "accept home-org scoping" escape hatch —
because trusted mode *means* "behind an edge doing N5." A single-workspace deployment is unaffected
for free (home == acting, so its edge always emits `acting`). Preserving D3, `runlet` checks only the
scope *label*; it never interprets the org relationship. Honest scope: this is a **contract tripwire,
not cryptographic proof** — the header rides the same trusted-edge boundary as `x-workspace-id`, so it is
only as strong as the NetworkPolicy. It defends against the *accidental* hazard (an edge without N5),
not a compromised edge, which the trust invariant already owns. In-box JWT verification was rejected:
it re-litigates the "no crypto in the box" decision and puts a JWKS-refresh surface on every
`/execute`. The header name is configurable (`trusted.headers.scope`, default `x-tenant-scope`).

**Runbook — bring-up ordering (producer before consumer):** the edge must emit `x-tenant-scope:
acting` **before** a box that enforces it is rolled out, or all trusted-mode traffic 403s. There is no
live traffic today (pre-users), so this is a fresh-deploy ordering note, not a migration: stand up the
N5-emitting edge first, then enable trusted mode on the box.

## Coarse member authorization

A config-driven `capability → required entitlement` map (`trusted.capability_entitlements`) gates
which capability a member may invoke, off the trusted `x-user-roles` / `x-user-entitlements`. This is
deliberately coarse ("may this member use `db` at all"), not fine-grained role→resource policy — that
is a v2 concern (revisit Cedar). A capability kind absent from the map is ungated. Runs before the
capability does.

## Per-tenant, plan-gated quota

`runlet` does per-tenant usage **accounting + a hard cap**; per-tenant request throttling rides the
edge (Envoy per-`x-workspace-id` rate-limit). The quota engine (`quota.rs`) mirrors the nexus
`routing-rs/plan.rs` shape — a data-driven `plan → limit` table, "at-or-above", **fail-closed**:

- A tenant's plan (from `x-tenant-plan`) selects a `PlanLimit` (today: `max_concurrent` in-flight
  executions per tenant).
- An **unknown/unconfigured plan** resolves to the most restrictive configured limit.
- An **empty** `plans` map (while `quota.enabled`) denies every request — a misconfiguration never
  grants unbounded usage.
- Over-limit returns a structured `429 QUOTA_EXCEEDED` carrying the plan, limit, and current usage.

## Request pipeline (trusted mode)

```
edge service credential  →  trusted identity (reject anonymous/suspended/tenant-less/non-acting-scope)
  →  partition = trusted tenant (caller-asserted ignored)  →  member-capability authz
  →  per-tenant quota admit  →  fabricd session (tenant-scoped)  →  Tier 5 + bulkhead  →  execute
```

## Out of scope

- Tenant-scoped script registry — the registry is platform-provided first-party scripts only;
  tenants submit inline `script`, so there is nothing per-tenant to isolate.
- Fine-grained role→resource policy (v2 / Cedar).
- Full `ring` eviction (needs quinn 0.12+).
