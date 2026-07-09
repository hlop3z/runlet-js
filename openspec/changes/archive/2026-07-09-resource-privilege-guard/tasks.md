# Tasks — resource-privilege-guard

> Design settled in design.md: D1 fail-closed refuse-at-boot; D2 startup preflight; D3 per-driver
> `privilege_concern()` for all six drivers; D4 no statement blocklist; D5 **enforcement in fabricd,
> contract + docs here**; D6 multitenant path forbids the `allow_privileged` opt-out — **derived from
> the trusted tenant identity the `WireInit` handshake already carries, with no new wire field and no
> box code change** (`least_privilege_required` would equal `tenant.is_some()` verbatim; rejected as
> redundant + policy-smearing). This repo ships the behavioral spec and the operator guidance; the
> fabricd sibling repo implements the preflight, the six probes, the config field, and the derivation.

## 1. Wire contract — no change (`crates/runlet-wire`)

- [x] 1.1 **No wire field.** Confirm `WireInit.tenant` is the sufficient signal: it is present exactly on the multitenant path, so fabricd derives the least-privilege mandate from it. Adding `least_privilege_required` was reverted (it equalled `tenant.is_some()`, carrying no new information and smearing policy onto the box). `WireInit` is untouched.
- [x] 1.2 Confirm the cross-repo wire contract is byte-identical to today (no field added/removed).

## 2. Box behavior — no change (`crates/runlet`)

- [x] 2.1 **No box code change.** The box already forwards the trusted tenant id in the handshake (`handler.rs::wire_init`); it sets no privilege flag and makes no privilege decision (kept dumb — forwards identity, never policy). Only a clarifying comment on `tenant` was added.
- [x] 2.2 Single-tenant/loopback path unchanged (no trusted identity ⇒ not a multitenant context).

## 3. Behavioral spec (already drafted — review only)

- [x] 3.1 Confirm `specs/tenant-egress/spec.md` delta MODIFIES the identity-carried requirement with the least-privilege-mandatory assertion and ADDS the multitenant-forbids-opt-out requirement
- [x] 3.2 Confirm no fabricd-internal behavior (preflight/probes/boot-gate) is specified here — it is `design.md` § Downstream (informative) only, to be lifted into fabricd's own proposal

## 4. Deployment security guidance — this repo owns the canonical doc

- [x] 4.1 Add a **least-privilege / trust-model** security section to `docs/design/resource-egress.md`: state the principle once — *a script gets exactly the privilege of the account behind the logical name* — and describe the fabricd preflight + fail-closed boot gate + the multitenant opt-out ban
- [x] 4.2 Add copy-pasteable **hardened-role recipes**, one per driver: `db` (`CREATE ROLE app_ro … NOSUPERUSER` + scoped `GRANT SELECT`/DML, revoke `pg_execute_server_program`/file roles); `redis` (`ACL SETUSER` scoped, not the unrestricted `default`); `mongo` (a scoped DB role, not `root`/`__system`/`dbOwner`); `mail` (a non-relay authenticated account); `amq` (a least-priv vhost user, no admin/management tag); `auth` (validation-only scope)
- [x] 4.3 Cross-reference the recipes from the boot-refusal remediation text described in the spec, so a refusal message can point operators here
- [x] 4.4 Update the beginner `docs/` capability guides only where they imply "point it at any account" — note the account should be least-privilege

## 5. Downstream tracking (fabricd sibling repo — OUT OF SCOPE here, do not implement)

- [x] 5.1 Ensure `proposal.md` records the fabricd deliverables (preflight, six `privilege_concern()` probes, `allow_privileged` per-resource config field + boot refusal, deriving the mandate from the `WireInit` tenant identity to void the opt-out on tenant-scoped sessions) as a tracked downstream dependency — no code lands in this repo
- [x] 5.2 Ensure `design.md` § Downstream (informative) preserves the full probe/preflight/gate design — including the coverage-regression guard (a driver with no probe ⇒ unverifiable ⇒ not served) — so fabricd's proposal can lift it intact

## 6. Wrap-up

- [x] 6.1 `task fmt` + `task clippy` clean (strict lint gauntlet); `cargo build` green (build/test via Docker per the env gotcha — WDAC blocks native cargo)
- [x] 6.2 `/opsx:sync` the delta specs into `openspec/specs/` (`tenant-egress` update — no separate capability spec; the least-privilege model is a tenant-egress modification), then `/opsx:archive`
