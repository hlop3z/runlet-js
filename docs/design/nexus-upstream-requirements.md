# nexus upstream requirements (cross-repo dependencies)

Requirements this project (`jsbox`/`runlet`) depends on the first-party **nexus** edge platform
(`dufeutech/nexus`) to satisfy. Tracked here as a dependency; the canonical home is the nexus repo's
own `nexus-upstream-requirements.md` — mirror any change there ("pin any rename in both repos").

## Downstream header contract (mirrored from nexus — what `runlet` codes against)

The edge strips all client-supplied `x-*` before the identity sidecar injects trusted headers.
Boxes treat these as authoritative and pre-authorized; they add **only resource-ownership checks**
(nexus ownership table: route protection, roles/plans, and AAL gates are edge concerns — N4 Phase 2 —
so boxes must not build their own role/plan gates for route access).

| Header                                                  | Meaning                                                         | Status                          |
| ------------------------------------------------------- | ---------------------------------------------------------------- | -------------------------------- |
| `x-workspace-id`                                        | the **authorized acting workspace** (live membership check)      | shipped                          |
| `x-user-id`                                             | the user, for audit                                              | shipped                          |
| `x-user-roles`, `x-user-entitlements`, `x-auth-method`  | enrichment inputs (edge enforcement lands with N4 Phase 2)       | shipped (injected, unenforced)   |
| `x-tenant-scope: acting`                                | acting-org tripwire; `runlet` fails closed without it            | **OPEN — N5, release gate**      |
| `traceparent`                                           | W3C trace context, edge-rooted                                   | open — N6, box fails open        |

**Naming, pinned 2026-07-02:** nexus renamed the tenant header — the sidecar injects
`x-workspace-id`; `x-tenant-id` survives only as a legacy read-fallback *inside* nexus and is never
emitted toward boxes. `runlet`'s trusted-header default is therefore `x-workspace-id`
(`trusted.headers.tenant`, still configurable). The scope header keeps `runlet`'s existing default
name `x-tenant-scope` — nexus emits it under that name.

## N5 — the identity plane must emit the *authorized acting org*, not the home org

`runlet` treats the `x-workspace-id` header as an opaque, **already-authorized acting-workspace id**
and keys all per-tenant isolation (fairness, cache, egress scope, quota) off it. For this to be
correct, the nexus identity plane must inject the tenant id as the **workspace the user is acting in
for this request** — selected and authorized upstream (an acting-scoped grant), opaque to the box —
**not** the user's home workspace.

- **Why:** a multi-org user acting in workspace B must be scoped to B's fairness bucket, cache
  namespace, egress bindings, and quota. If the edge emits the home org A instead, the user is
  mis-scoped — reaching A's resources and quota while acting in B, or being denied B's.
- **Contract:** `x-workspace-id` = the authorized acting org for this request; `x-user-id` = the
  user (audit); the edge strips any client-supplied `x-*` before injecting these.
- **Acting-org assurance (enforced box-side):** on every authorized-acting-org request the edge
  SHALL also emit a trusted `x-tenant-scope: acting` header. `runlet` **enforces** this
  fail-closed: in trusted-header mode a tenant-scoped `/execute` whose `x-tenant-scope` is absent or
  not equal to `acting` is rejected `403 ACTING_SCOPE_REQUIRED` before any egress session or
  execution. This turns a silent multi-org mis-scope (an edge that has not shipped N5, or has
  drifted) into a loud rejection. It is a **contract tripwire, not cryptographic proof** — the header
  rides the same trusted-edge boundary as `x-workspace-id`, so it is only as strong as the
  NetworkPolicy (see D3). The header name is configurable box-side (`trusted.headers.scope`, default
  `x-tenant-scope`); pin any rename in both repos.
- **Bring-up ordering (producer before consumer):** the edge must emit `x-tenant-scope: acting`
  **before** a box that enforces it is rolled out, or all trusted-mode traffic 403s. There is no live
  traffic today (pre-users), so this is a fresh-deploy ordering note, not a migration.
- **Scope of impact:** single-workspace users are unaffected (their acting org is their only org, so
  the edge always emits `acting`); the requirement gates the **multi-org** case.
- **Status (2026-07-02):** box side **enforced**. Nexus side: the *semantics* shipped — the identity
  sidecar authors `x-workspace-id` from a **live membership check** of the resolved workspace, never
  the token's `resourceowner` (the home org is retired as an authz input). The **tripwire emission**
  (`x-tenant-scope: acting`) is still open in nexus and remains the multi-org release gate — until
  the edge emits it, trusted-mode traffic is rejected, so bring the edge up first.

Related: `docs/design/multitenant-trust.md` (decision D3; the acting-org gate).

## N6 — the edge must propagate a W3C `traceparent` so edge→box→`fabricd` is one trace

`runlet` emits an OpenTelemetry span per `/execute` (tenant/user/plan as span **attributes**, never
metric labels) and exports it OTLP to a collector. For a request's trace to span the whole path,
the nexus edge must start the trace and inject a standard **W3C `traceparent`** (and optionally
`tracestate`) header, which the box reads and **continues** (parent-based sampling honors the
edge's sample decision).

- **Why:** without a propagated `traceparent`, each box starts its own orphan root span — traces
  still work but cannot be tied back to the edge request or correlated across hops. With it, one
  trace id threads edge → box → (later) `fabricd`.
- **Contract:** the edge SHALL inject `traceparent` per the W3C Trace Context spec on requests it
  forwards to the box; it makes the head sampling decision. The box does no tail sampling (that is
  the collector's job).
- **Graceful degradation (no hard dependency):** if the edge does not emit `traceparent`, the box
  starts its own root span and applies its configured sample ratio — so this is an *enhancement*,
  not a release gate. `meta.trace_id` is the propagated id when present, else the box-rooted id.
- **Bring-up ordering:** stand up the collector and set `telemetry.otlp_endpoint` on the box, then
  enable edge propagation — the box tolerates any order (fail-open, D6).
- **Status:** box side implemented (continues `traceparent`, fail-open); nexus side open — the edge
  has no tracing config today and monitoring is metrics-only. Track as an observability
  enhancement, not a gate.

Related: `docs/design/multitenant-trust.md`; the `observability` spec (distributed tracing).
