## Why

A live pentest of the `box → fabricd → Postgres` path showed that a `fabricd` resource bound to
an over-privileged backend account grants any sandboxed script the full power of that account: a
`db` resource using a Postgres **superuser** role let a script run `COPY (SELECT 1) TO PROGRAM
'id'` — real command execution on the database host (verified `uid=70(postgres)`). The identical
attack against a least-privilege read-only role was blocked by the backend's own grants. This is
not a box defect — the box holds no credentials and faithfully forwards SQL; the trust boundary is
the *privilege of the account behind the logical name*, which only `fabricd` can see. The hazard
generalizes to every driver (redis no-ACL → `CONFIG SET`+`SAVE` webshell; mongo `root` → all DBs +
server-side JS; open mail relay; amq admin). On the multitenant/nexus path an over-privileged
resource is catastrophic — one tenant's script reaches every tenant's data and the backend host —
so the guard must be multitenant-first and fail-closed by default.

**Scope note.** The enforcement (the startup preflight, the six privilege probes, the
`allow_privileged` config field, the boot refusal) is fabricd-internal behavior the box never
observes, so it lives in fabricd's own planning — **not specified here**. This change is the
**box-side contract seam plus the operator guidance** that this repo owns: the wire signal that lets
the box tell fabricd "untrusted tenants are behind this session," and the canonical least-privilege
docs (`resource-egress.md` is pinned canonical in this repo). See the *Downstream* impact below.

- Specify that a session carrying a trusted tenant identity **is** the untrusted-tenant
  (multitenant) context, so fabricd **forbids the `allow_privileged` override entirely on the
  multitenant path** (an operator cannot opt a privileged resource into a multitenant deployment).
  The box already forwards the trusted tenant id in the `WireInit` handshake, and fabricd derives the
  mandate from that identity — **no new wire field, no box code change.** (A separate
  `least_privilege_required` bool would equal `tenant.is_some()` verbatim, carrying no information
  fabricd doesn't already hold and smearing a policy decision onto the box; rejected — see
  `design.md` D6.)
- Add **deployment security guidance**: a least-privilege section in `docs/design/resource-egress.md`
  with copy-pasteable per-driver hardened-role recipes, plus the trust-model principle stated once —
  *a script gets exactly the privilege of the account behind the logical name.* This is the canonical
  home for the guidance fabricd's boot-refusal message points operators at.

## Capabilities

### Modified Capabilities
- `tenant-egress`: on the multitenant path (a trusted tenant identity present in the `WireInit`
  handshake), any per-resource `allow_privileged` override SHALL be forbidden — a resource must be
  least-privilege to be served to any tenant. Clarifies the existing
  `WireInit`-carries-tenant-identity contract: the tenant identity's **presence** is itself the
  multitenant marker fabricd keys the mandate off — no additive wire signal, no box behavior change.

## Impact

- **This repo (ships the behavioral spec + docs only — no code change):**
  - `crates/runlet-wire` / `crates/runlet` — **unchanged.** The multitenant context is the trusted
    tenant identity the `WireInit` handshake already carries; no new field, no box behavior change.
  - `docs/design/resource-egress.md` — new security section + per-driver hardened-role recipes.
  - `openspec/specs/` — `tenant-egress` delta only (no new capability spec lands here).
- **`fabricd` sibling repo (downstream — its own change, tracked not implemented here):** the startup
  preflight, the six per-driver `privilege_concern()` probes, the `allow_privileged` per-resource
  config field + fail-closed boot refusal, verdict logging with remediation, and deriving the
  least-privilege mandate from the `WireInit` **tenant identity** (a tenant-scoped session voids the
  opt-out). The probe/gate design is captured **informatively** in this change's `design.md`
  (§ Downstream) to be lifted into fabricd's proposal; it is not normative here.
- **Operators:** must ensure each resource's backend account is least-privilege (or set
  `allow_privileged: true` on a non-multitenant deployment) before upgrading fabricd. **BREAKING
  (operational, downstream):** once fabricd ships the guard, an over-privileged resource fails fabricd
  boot until hardened or acknowledged. No wire/HTTP change in this repo at all (the box code and the
  `WireInit` contract are untouched).
