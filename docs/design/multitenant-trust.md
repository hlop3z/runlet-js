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
| plan (quota tier)  | `x-workspace-plan`     | `plan`                    |
| acting-org scope   | `x-tenant-scope`       | `scope` → must be `acting` (N5) |

Every name is configurable (`trusted.headers.*`) so a drift between the edge contract and the box is
pinned in one place. The tenant default is `x-workspace-id` — the name the nexus identity sidecar
injects (`x-tenant-id` survives only as a legacy read-fallback *inside* nexus and is never emitted
toward boxes); the pinned contract is the "Downstream header contract" table in
`nexus-upstream-requirements.md`. Trusted mode is **opt-in** (`trusted.enabled`); the default preserves the
pre-change single-principal, loopback behavior.

> **Drift note — the bare sensitive headers are the *plain-path* source only.** nexus's "B-floor trust
> hardening" **retired** the bare `x-user-roles` / `x-user-entitlements` / `x-user-suspended` headers and
> now carries those signals **only** inside the signed `x-identity-contract`, stripping the bare copies.
> The table above describes the **plain trusted-header path** (a generic / pre-hardening edge). Behind
> current nexus, enable the **[signed-contract sub-mode](#signed-contract-sub-mode-opt-in)** below, which
> sources roles/entitlements/suspended/plan from the *verified claims* instead — otherwise `suspended`
> reads a header nexus no longer sends and fails open. The plan header was also renamed
> `x-tenant-plan` → `x-workspace-plan` to match what nexus emits.

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
  `x-user-id` value; the *key id* for an api-key principal), so a consumer can build a who-did-what audit
  trail. Emitted only when a trusted subject is present.
- **`X-Runlet-Principal-Kind`** (*what authenticated* — `user`/`apikey`/`service`) and
  **`X-Runlet-On-Behalf-Of`** (*the human behind an api-key*) — sourced from the **verified signed
  contract**, so a consumer can branch a `service` writer vs a human and attribute an api-key action to
  **both** the key (`X-Runlet-Actor`) and the human. Emitted **only** when a verified contract populated
  them — absent on the plain trusted-header path and single-tenant.

All are sourced **only** from the trusted-identity extractor. A routing key like an event stream may
ride the untrusted `payload`, but an actor is a *trust assertion* and therefore must be out-of-band —
the payload path is off-limits for it.

Principal **kind** and **on-behalf-of** ride **both** transports on equal terms with tenant/actor: the
box-direct headers above, and the broker's `WireInit.principal_kind` / `WireInit.on_behalf_of` fields
(each `Option`, skipped when absent). They are forwarded **only** when a verified contract populated them,
so a present `principal_kind` on the wire is always a *verified* fact, never a bare-header assertion;
`X-Runlet-Actor` / `WireInit.actor` stay stably equal to the bare subject. (Earlier this design forwarded
neither, because the box verified no signed assertions — the signed-contract sub-mode changed that.)
**Gating** on kind (admit a `service` writer vs a human) remains a separate, deferred concern. See
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
not a compromised edge, which the trust invariant already owns. The header name is configurable
(`trusted.headers.scope`, default `x-tenant-scope`).

In the **[signed-contract sub-mode](#signed-contract-sub-mode-opt-in)** a *verified* `x-identity-contract`
**is** the acting-org assurance — nexus mints it only for a caller resolved to an authorized acting
workspace — so the scope-header tripwire is **not** additionally required on that path (the plain path
above keeps it). What was once a blanket "no crypto in the box" rule is now an **opt-in** verifier: the box
still ships zero signing keys and adds no second crypto stack (ES256 verify rides the existing aws-lc-rs
provider), and the JWKS-refresh surface exists only when an operator turns the sub-mode on.

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

- A tenant's plan (from `x-workspace-plan`, or the verified contract's `plan` claim in the sub-mode)
  selects a `PlanLimit` (today: `max_concurrent` in-flight executions per tenant).
- An **unknown/unconfigured plan** resolves to the most restrictive configured limit.
- An **empty** `plans` map (while `quota.enabled`) denies every request — a misconfiguration never
  grants unbounded usage.
- Over-limit returns a structured `429 QUOTA_EXCEEDED` carrying the plan, limit, and current usage.

## Signed-contract sub-mode (opt-in)

nexus's B-floor hardening moved the revocation-sensitive signals (`roles`, `entitlements`, `suspended`)
out of bare headers into a signed **`x-identity-contract`** (an ES256 JWS) and strips the bare copies. The
plain trusted-header path above reads headers nexus no longer sends — so behind current nexus a box must
verify the contract instead. That is the **`trusted.contract`** sub-mode: opt-in, layered *alongside* the
plain path (a non-nexus / pre-hardening edge that injects plain headers keeps working when it is off).

**What it does when enabled** (config: `trusted.contract.{enabled, jwks_url, issuer, audience,
supported_ctr, leeway_secs, min_refresh_secs}`; a boot guard refuses to start with `jwks_url`/`issuer`/
`audience` unset):

- **Verify the JWS** (`contract.rs`, `ContractVerifier`): fetch the JWKS from `jwks_url` (nexus serves
  `<identity-plane>/.well-known/jwks.json`), select the key by the token's `kid`, refresh on an unknown
  `kid` (bounded by `min_refresh_secs`, last-good kept across a transient outage). ES256 verify reuses the
  process aws-lc-rs provider (`jsonwebtoken`'s `aws_lc_rs` backend) — **no `ring`, no second crypto
  stack, verify-only**, the box holds no signing key.
- **Check the registered claims**: `iss` == configured issuer, `aud` == this box's pool name (nexus scopes
  each token to one box — a token for another box is rejected), `exp` in the future (± `leeway_secs`). A
  repeated `jti` is **not** a replay (nexus reuses one contract within a short window).
- **`ctr` drift gate**: reject a contract whose version is outside `supported_ctr` (default `["v1"]`) — an
  incompatible contract-shape change fails **loud** instead of being silently mis-read. This is the alarm
  that would have caught the very drift this change fixes.
- **Source identity from the verified claims** (authoritative over any bare header): tenant
  (`workspace_id`), user (`sub`), roles, entitlements, plan, plus the newly-captured `principal_kind` and
  `on_behalf_of`. **Absent `suspended` is treated as unknown → deny**, never `false` (the fail-open bug
  this change closes). A verified contract is not reused past its `exp`.
- **Acting-org**: a verified contract **replaces** the `x-tenant-scope: acting` tripwire (it is minted
  only for a resolved acting workspace); the scope header is not additionally required on this path.
- **Fail closed**: on this always-enriched route, a missing or unverifiable contract is a `403` (distinct
  audit reasons: `CONTRACT_MISSING` / `CONTRACT_MALFORMED` / `CONTRACT_INVALID` / `CONTRACT_UNKNOWN_KEY` /
  `CONTRACT_KEYS_UNAVAILABLE` / `CONTRACT_VERSION_UNSUPPORTED`), never an anonymous-but-allowed request.

Non-sensitive gateway fields nexus carries as plain `x-runlet-*` headers (mode / capture / log-level) are
**not** claims and are still read from headers in the sub-mode. The build-vs-adopt record for the verify
crate (`jsonwebtoken`/`aws_lc_rs`) and the JWKS-cache build is in the change's `design.md` (D6/D7).

Forwarding `principal_kind`/`on_behalf_of` downstream (broker `WireInit` + box-direct headers) is
**done** — see "Identity on the egress paths" above. **Open (deferred):** *gating* on `principal_kind`
(admit a `service` writer vs a human); whether `supported_ctr` should be a min-version range.

## Request pipeline (trusted mode)

```
edge service credential  →  trusted identity
     plain path:     reject anonymous / suspended / tenant-less / non-acting-scope (header reads)
     contract path:  verify x-identity-contract (sig · iss · aud · exp · ctr) → claims;
                     reject missing/invalid; suspended absent ⇒ deny; scope satisfied by the contract
  →  partition = trusted tenant (caller-asserted ignored)  →  member-capability authz
  →  per-tenant quota admit  →  fabricd session (tenant-scoped)  →  Tier 5 + bulkhead  →  execute
```

## Out of scope

- Tenant-scoped script registry — the registry is platform-provided first-party scripts only;
  tenants submit inline `script`, so there is nothing per-tenant to isolate.
- Fine-grained role→resource policy (v2 / Cedar).
- Full `ring` eviction (needs quinn 0.12+).
