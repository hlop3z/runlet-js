## Why

The box already forwards the request's trusted **tenant** to a box-direct loopback service
(`X-Runlet-Tenant`, the sibling `box-direct-tenant-header` change), so a co-located service can scope
data by acting workspace. But it stops at *where* — it never forwards *who*. The box already holds the
acting subject (`TrustedIdentity.user`, from the trusted `x-user-id` header) and throws it away at the
egress boundary. A box-direct consumer that wants a who-did-what audit trail (an event-log-style write
target) has no trusted way to learn the actor: stream/routing keys can ride the untrusted script
`payload`, but an actor identity is a **trust assertion** and MUST arrive out-of-band, exactly like the
tenant. Today it cannot.

## What Changes

- The box-direct POST SHALL carry the request's trusted acting subject as an out-of-band HTTP header
  (`X-Runlet-Actor`) when — and only when — a trusted subject is present. The value is the **bare
  subject** (`TrustedIdentity.user`); the `{action, payload}` request **body** is unchanged (D9).
- The header is sourced **only** from the trusted-identity extractor (`TrustedIdentity.user`), never
  from anything the executing script or caller can influence. On the single-tenant / non-trusted path
  (no trusted subject), **no** header is added and behavior is byte-for-byte unchanged.
- Scope is **box-direct only** and **subject only**. `http`/`s3` carry no identity (unchanged). The
  broker/`WireInit` path is unchanged (touching it would change the cross-repo `runlet-wire` contract).
  Principal **kind** (user/apikey/service) is deliberately **not** forwarded — it is crypto-gated
  (signed `x-identity-contract` only), and the "no crypto in the box" invariant stands; if a consumer
  ever needs kind, it arrives later as a *separate* trusted header, leaving `X-Runlet-Actor` stably
  equal to the subject.
- Documentation is brought current alongside the code: `docs/design/multitenant-trust.md` gains the
  cross-repo vocabulary equivalence (nexus `Workspace` == runlet `tenant` == event-logs `Tenant`), a
  one-line note on which identity axes the box keys on vs. carries vs. ignores, and the box-direct
  actor-forwarding rule; the stale "ZITADEL org" characterization is dropped in the two live docs that
  assert it (`multitenant-trust.md`, `nexus-upstream-requirements.md`) in favor of the box's own
  opacity doctrine.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `tenant-egress`: the "Tenant identity carried on the box-direct local egress path" requirement gains a
  companion — the box-direct call also carries the trusted **acting subject** as an out-of-band header
  (`X-Runlet-Actor`), on the same trusted-only, script-proof, emit-only-when-present terms as
  `X-Runlet-Tenant`, without altering the `{action, payload}` envelope, and scoped to box-direct only.

## Impact

- **Code (this repo only):** `crates/runlet/src/local_io.rs` (`BoxEgress` stores the actor; `build_request`
  emits the header), `crates/runlet/src/handler/mod.rs` (`ExecuteBlocking` gains an `actor` field), and
  its build sites `crates/runlet/src/handler/lifecycle.rs` + `handler/batch_items.rs` (pass the
  already-computed `identity.user`, exactly as `tenant` is threaded).
- **Docs:** `docs/design/multitenant-trust.md`, `docs/design/nexus-upstream-requirements.md`.
- **No wire-contract change:** `runlet-wire` is untouched; box-direct is plain HTTP to a loopback service.
- **Downstream:** a co-located loopback service MAY now read `X-Runlet-Actor` for audit; services that
  ignore it are unaffected (additive header). No consumer exists in the sibling event-logs repo yet — its
  write path is tenant+stream only today — so this ships as a trusted producer ahead of a consumer, by
  the user's decision, to have the actor already on the wire when a consumer field lands.
- **Out of scope:** `http`, `s3`, the broker/`WireInit` path, principal kind, roles/entitlements/plan.
