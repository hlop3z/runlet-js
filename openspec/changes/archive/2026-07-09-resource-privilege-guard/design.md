## Context

The box (`runlet`) links no driver and holds no credentials; when a request names a driver
resource, `fabricd` resolves the logical name against its operator credential table and performs
the I/O. A pentest of this path demonstrated that a resource pointed at an over-privileged backend
account gives any sandboxed script that account's full power — a Postgres **superuser** resource
yielded `COPY … TO PROGRAM` command execution on the database host; the same script against a
least-privilege role was blocked by the backend's own grants.

The box cannot see or fix this: it never connects and never sees a role. Only `fabricd` — which
holds the credentials and opens the connection — can inspect a resource's privilege. But
`docs/design/resource-egress.md` and the `runlet-wire` protocol are canonical **in this repo**, so
this repo owns the *contract and the guidance*; the `fabricd` sibling repo owns the *enforcing
code*. This change lands the contract, the wire signal, and the docs; fabricd implements against
them.

The hazard is trust-model dependent: acceptable-ish for a solo internal tool where author ==
operator, catastrophic on the multitenant/nexus path where untrusted tenants share one `fabricd`.
The design is therefore multitenant-first and secure-by-default.

## Goals / Non-Goals

**Goals:**
- Make an over-privileged egress resource a **loud boot failure**, not a silent latent RCE.
- Cover **all six drivers** (db/redis/mongo/mail/amq/auth) with one privilege-probe abstraction.
- Be **fail-closed by default** with an explicit, per-resource operator acknowledgement to opt out.
- On the **multitenant path**, remove the opt-out entirely — least-privilege is mandatory.
- Give operators a **copy-pasteable remediation** (hardened-role recipe per driver) so the secure
  path is the easy path.

**Non-Goals:**
- Inspecting or rewriting the SQL/commands a script issues (no statement blocklist — see Decisions).
- Runtime per-request privilege checks (the probe is a startup preflight, not a hot-path gate).
- Implementing the probes/preflight here — that is fabricd's downstream work against this contract.
- Changing which resources a tenant may *name* (that is `tenant-egress`'s scoping, untouched here).

## Decisions

> **Normative scope.** Only **D5** (the enforcement/contract split) and **D6** (the multitenant
> opt-out ban) are normative *in this repo* — they define the contract this repo owns (the behavioral
> spec + the operator docs). **The box code does not change:** the multitenant context is derived from
> the trusted tenant identity the handshake already carries — no new wire field (see D6). **D1–D4**
> describe fabricd-internal behavior (its boot preflight, probes, and gate); they are captured here
> **informatively**, to be lifted into fabricd's own proposal. The box never observes them.

### D1 — Fail-closed refuse-at-boot, not warn-only *(informative — fabricd)*
`fabricd` refuses to serve (refuses to boot) when a resource is over-privileged and the operator has
not set `allow_privileged: true`. **Why over warn:** a warning in a log is ignored; an over-privileged
resource is a full compromise on the multitenant path. This mirrors the box's existing fail-closed
posture (`allow_unauthenticated` on a non-loopback bind; the trusted-mode network-isolation assertion)
— removing a guard requires an explicit, recorded operator acknowledgement. *Alternative rejected:*
warn-by-default in solo mode — leaves the catastrophic default un-guarded and relies on discipline.

### D2 — Startup preflight, not lazy first-use probe *(informative — fabricd)*
The probe runs once per resource at startup (connect → probe → disconnect). **Why over lazy:**
deterministic boot validation fails loud before any traffic; a lazy "warn on first use" never checks
an unused-but-present resource and pays the probe cost on a latency-sensitive first request.
*Trade-off:* startup connects to every backend once — a backend briefly down at boot must be handled
(see Risks). *Alternative rejected:* probe-and-cache on first connect — weaker guarantee, non-deterministic.

### D3 — A per-driver `privilege_concern()` seam, not a Postgres-only check *(informative — fabricd)*
Each driver implements a probe returning an optional structured concern:
- **db** → `SHOW is_superuser` / `pg_roles.rolsuper` (+ role membership of `pg_execute_server_program`,
  `pg_read_server_files`, `pg_write_server_files`).
- **redis** → `ACL WHOAMI` + `ACL GETUSER` — flag the `default` user with no ACL, `~*`/`+@all`, or
  `CONFIG`/`MODULE`/`DEBUG`/`SCRIPT` reachable.
- **mongo** → `connectionStatus` / `rolesInfo` — flag `root`, `__system`, `dbOwner`/`dbAdminAnyDatabase`
  on `admin`, or cluster-wide roles.
- **mail** → authenticated non-relay check (reject an account that relays to arbitrary recipients).
- **amq** → management/administrator tag or full-vhost configure/write/read on `/`.
- **auth** → introspection/management scope broader than token validation.
**Why:** the hazard is identical across drivers ("a script gets the account's privilege"); a
Postgres-only `is_superuser` band-aid would leave five equivalent holes. *Alternative rejected:*
ship db-only now — under-serves the multitenant-first goal.

### D4 — No SQL/command statement blocklist *(informative — fabricd)*
We do **not** deny specific statements (e.g. `COPY … PROGRAM`). **Why:** a blocklist is a perpetual
arms race (`COPY`, `lo_export`, `CREATE FUNCTION` C-lang, `dblink`, …), it breaks the "the box
faithfully forwards SQL" contract that lets legitimate scripts do real work, and it does not
generalize across drivers. The correct control is the backend account's own privilege. *Rejected.*

### D5 — Enforcement in fabricd, contract + docs here
The probe needs live credentials and a connection, which live only in `fabricd`. But the design doc
(`resource-egress.md`) and the wire protocol (`runlet-wire`) are canonical in this repo. So: the
**behavioral spec, the `WireInit` field, and the operator guidance** land here; the **preflight,
the six probes, and the `allow_privileged` config field** land in the fabricd repo. Tracked as a
downstream dependency in `proposal.md`.

### D6 — Multitenant path forbids the `allow_privileged` override *(no new wire field)*
When the box opens an egress session carrying a trusted tenant identity (multitenant/nexus mode),
`fabricd` treats `allow_privileged: true` as void for that session and refuses to serve a flagged
resource regardless of config. **Why:** the opt-out exists for a trusted solo operator accepting the
risk for their own scripts; it must never silently weaken a deployment that serves untrusted tenants.
**How the daemon knows:** the trigger is the trusted tenant identity the handshake **already**
carries (`WireInit.tenant`) — a tenant-scoped session *is* the multitenant context. We add **no new
wire field**: `least_privilege_required` would be exactly `tenant.is_some()`, carrying zero
information the daemon does not already hold, and it would make the box appear to make a privilege
decision it does not make. Keeping the box dumb (it forwards identity, never policy) is the whole
point of the egress split; the cross-repo wire contract is unchanged. *Alternative rejected:* an
explicit additive `least_privilege_required: bool` — redundant with `tenant` presence (YAGNI), and it
smears policy semantics onto the box's forward-only handshake.

## Risks / Trade-offs

- **Backend unreachable at boot** → the preflight cannot distinguish "down" from "misconfigured."
  Mitigation: refuse to boot on an *affirmative* over-privileged verdict only; on an inconclusive
  probe (connection failure) fail loud with a distinct "could not verify" error rather than silently
  passing — an unverifiable resource is not served. Operators keep the existing per-resource opt-out
  for backends that legitimately reject the probe.
- **Probe false-positives** (a superuser role that is nonetheless network-isolated and single-purpose)
  → the per-resource `allow_privileged: true` opt-out is the escape hatch (solo path only, per D6).
- **Upgrade breakage** (existing over-privileged deployments fail to boot) → this is the intended
  fail-closed behavior; mitigated by a clear remediation message + the docs recipes, and by the
  per-resource opt-out for operators who cannot re-role immediately.
- **Probe cost / driver coverage drift** → a new driver added to fabricd without a `privilege_concern()`
  must default to "cannot verify ⇒ not served" so coverage can never silently regress.
- **Cross-repo skew** → the `WireInit` field is additive and defaulted; fabricd honoring it is a
  separate PR. Until fabricd ships, the box-side signal is inert (no regression), and the boot guard
  is simply absent — the docs guidance (A) still applies.

## Migration Plan

1. Land the contract here (spec + `WireInit` field + docs recipes). Box-side signal is inert until
   fabricd honors it — no behavior change on upgrade of the box alone.
2. fabricd implements the preflight + six probes + `allow_privileged` config field + the
   `WireInit`-signal handling (downstream PR in the sibling repo).
3. Operators, before upgrading fabricd: harden each resource's backend role to least-privilege using
   the recipes, or set `allow_privileged: true` on non-multitenant resources.
4. Rollback: an operator blocked by a false-positive sets `allow_privileged: true` (solo path) or
   pins the prior fabricd until the role is hardened. No wire/HTTP rollback needed (additive field).

## Open Questions

- Exact over-privileged thresholds per driver (e.g. is `dbOwner` on the app DB acceptable, or only
  scoped read/DML?) — to be pinned per driver in fabricd's implementation against the spec's intent.
- Whether an inconclusive probe should be overridable by a separate `allow_unverified: true` distinct
  from `allow_privileged`, or fold into the same opt-out.

## Build-vs-Adopt Decisions

Recorded by the `/opsx:decide` gate. Only the **wire-signal** decision is normative in this repo (it
is the box-side contract). The **probe** and **boot-gate** decisions are fabricd-internal, captured
here informatively for the fabricd implementer to inherit. Tool names live here only — `specs/` and
`config.yaml` stay abstract.

### Decision: Per-driver privilege probe — Build hand-written probes *(informative — fabricd)*

- **Status**: approved
- **Why**: No tool does inline, at-boot, cross-driver privilege determination; each probe is ~10–30
  lines of standard, stable catalog introspection run **through the vendor driver fabricd already
  links**, so "build" here adopts the driver's introspection surface rather than reimplementing a
  protocol.
- **Considered**: Adopt an external audit/CSPM scanner (pgDSAT/CIS-style) — PG-centric, no uniform
  six-driver coverage, spawns a process / adds a heavy dep at boot, built for periodic posture reports
  not a fast connect→probe→disconnect preflight. RBAC crates (`role-system`, `privilege`) — model the
  app's own authz, not a remote backend's account; irrelevant.
- **Isolation**: the per-driver `privilege_concern()` trait seam (D3) in fabricd — one impl per driver,
  each returning a structured `Concern`; a driver with no impl defaults to unverifiable (coverage
  cannot silently regress).

### Decision: Fail-closed boot gate — Build (mirror existing box guards) *(informative — fabricd)*

- **Status**: approved
- **Why**: Refuse-to-boot orchestration with a per-resource acknowledgement is control flow, not a
  library concern; it mirrors the box's existing fail-closed boot guards (`allow_unauthenticated` on a
  non-loopback bind; the trusted-mode network-isolation assertion), so it reuses an established in-repo
  pattern rather than inventing one.
- **Considered**: none warranted — there is no "boot-gate" tool to adopt; the alternative is warn-only
  (rejected in D1).
- **Isolation**: fabricd's startup preflight loop (D2) over the resource table, gated per resource by
  the `allow_privileged` config field and the `WireInit` least-privilege-mandatory signal (D6).

### Decision: Least-privilege signal — Reuse the existing `WireInit.tenant`, add no field

- **Status**: approved
- **Why**: The multitenant context the mandate needs is *already* on the wire — `WireInit.tenant` is
  present exactly when the session serves an untrusted tenant. A separate `least_privilege_required`
  bool would equal `tenant.is_some()` verbatim: zero new information, and it would smear a policy
  decision onto the box, whose job in the egress split is to forward identity and never decide policy.
  So the box code does not change at all; `fabricd` derives the mandate from the tenant id it already
  receives. Nothing is built or adopted here.
- **Considered**: an additive, defaulted `least_privilege_required: bool` on `WireInit` — rejected as
  redundant with `tenant` presence (YAGNI) and as leaking policy semantics into the box handshake. A
  new handshake frame / protocol-version bump — even more unjustified.
- **Isolation**: none needed in this repo — the box surface is untouched. The derivation lives in
  `fabricd` (downstream), keyed off `WireInit.tenant`.
