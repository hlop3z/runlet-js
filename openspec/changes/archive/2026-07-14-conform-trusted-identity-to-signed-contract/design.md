## Context

The box runs `/execute` behind the nexus edge in trusted-header mode. Its identity layer
(`crates/runlet/src/identity.rs`, `handler/gates.rs`, `authz.rs`, `quota.rs`) was built against
nexus's pre-hardening header contract. Nexus has since shipped "B-floor trust hardening": the
revocation-sensitive signals (`roles`, `entitlements`, `suspended`) were **removed from bare headers
and moved into a signed `x-identity-contract` (ES256 JWS)**, which nexus now also strips from client
input on every path. Nexus verified facts (from its `identity-rs` source):

- The contract is a compact **ES256 JWS**; claims include `iss`, `aud` (the destination box's pool),
  `exp`, `jti`, `ctr` (contract version, currently the string `"v1"`), `sub`, `workspace_id`,
  `principal_kind`, `on_behalf_of?`, `member_type?`, `role?`, `roles`, `permissions?`,
  `entitlements?`, `suspended?`, `plan?`. `entitlements`/`suspended`/`plan` are **omitted when
  unresolved** (absence = unknown).
- JWKS is served at `<identity-plane>/.well-known/jwks.json`; keys rotate with overlap; select by
  `kid`, refresh on unknown `kid`.
- Nexus mints a contract **only** for a resolved authority, reuses one contract across requests
  within a window (so `jti` is not a per-request nonce), and scopes replay by `aud` + short `exp`.
- Nexus *signs* with `jsonwebtoken 9`, which pulls **`ring`** — nexus explicitly rejects
  `aws-lc-sys` (license + C toolchain). The box has the **opposite** hard invariant: aws-lc-rs only,
  `ring`-free, `cargo tree -i ring` empty (tied to the mimalloc/musl posture in the box's memory).

The mismatch produces a live security defect (suspension fails open), a plan-header rename bug, and
a scope-header gate that would 403 every nexus request. See `proposal.md`.

## Goals / Non-Goals

**Goals:**

- Verify the signed `x-identity-contract` and source identity from its claims when an **opt-in**
  `trusted.contract` sub-mode is enabled.
- Adopt nexus's `ctr` claim as a **loud drift gate** (reject unknown versions).
- Fix the fail-open suspension (absent `suspended` ⇒ deny), the plan header, and the scope gate.
- Preserve the generic trusted-header path so a **non-nexus edge can still stand in** (sub-mode off).

**Non-Goals:**

- No change to nexus (it already ships the contract) or to the box's egress/forwarding wire
  (`x-runlet-tenant`/`x-runlet-actor` downstream stay as-is — the box re-vouches on that hop; the
  `aud`-scoped contract cannot be forwarded).
- Event-logs tenant isolation and the `X-Runlet-Tenant` ingest route are **out of scope** (follow-up).
- The canonical cross-repo vocabulary artifact (`Nexus-IDS.md` / `nexus-upstream-requirements.md`) is
  **out of scope** (follow-up).
- No signing capability in the box — **verify only** (public key from JWKS).

## Decisions

### D1 — Opt-in sub-mode layered on the existing header path (settled)

Contract verification is a new `trusted.contract` config block, active only when enabled. When off,
identity derivation is byte-for-byte the current behavior. **Why:** the box's stated value is "any
trusted edge can stand in"; hard-coupling every trusted deployment to a nexus-shaped JWKS would
break that. Alternative (replace header trust outright) rejected for that reason.

### D2 — Verify only, never sign; claims are authoritative when present (settled)

The box holds a public verifier, not a signer. When a verified contract is present its claims win
over any plain header; non-sensitive plain headers (`x-workspace-id`/`x-user-id`/`x-user-type`) may
still be read as a convenience but never override a claim. **Why:** matches nexus's model (bare
sensitive headers are retired; the signed value is the truth) and keeps the trust surface to one
place.

### D3 — `ctr` is a reject-list gate (settled)

Configurable `supported_ctr` (default `["v1"]`); a contract whose `ctr` is outside the set is
rejected with a version-mismatch reason. **Why:** nexus designed `ctr` as "the single coordination
gate for the header family's shape." Failing loud on an unknown version is the whole point — it
converts the next silent drift into an immediate, attributable rejection.

### D4 — Absent-is-unknown for revocation signals (settled)

`suspended`/`entitlements` omitted ⇒ **unknown ⇒ fail safe (deny)**, never treated as
false/empty-allow. `plan` omitted ⇒ not-provisioned (grant no tier), consistent with today's
quota fail-closed. **Why:** this is the exact defect being fixed; nexus's own contract doc mandates
"treat absent `suspended` as unknown, never false."

### D5 — Fail closed on an enriched route (settled)

With the sub-mode on, a missing/unverifiable contract on an identity-enriched route is a rejection,
not an anonymous pass. Nexus mints a contract for every authorized caller and strips forged copies,
so absence means unauthorized. **Why:** matches nexus's box-consumer contract; avoids a bypass.

### Decision: D6 ES256 JWS verification — Adopt `jsonwebtoken` (aws_lc_rs backend)

- **Status**: approved
- **Why**: The security-critical logic (JOSE parse, `alg` pinning that rejects alg-confusion/`none`,
  and `exp`/`aud`/`iss` validation) is exactly what we must not hand-roll; `jsonwebtoken` is the
  audited de-facto standard (the same family nexus signs with). Depend on it as
  `default-features = false, features = ["aws_lc_rs"]` — the `aws_lc_rs` backend reuses the aws-lc-rs
  library **already in-tree** via `rustls-aws-lc-rs`, so it pulls **no `ring`** and adds no second
  crypto stack (`cargo tree -i ring` stays empty). Verify with `Validation::new(Algorithm::ES256)`
  (alg pinned before verify) and validated `aud`/`iss`/`exp`; ES256 keys are built from the JWK's
  `x`/`y` coordinates via `DecodingKey::from_ec_components`.
- **Considered**: (b) pure-Rust `p256`/RustCrypto + a BYO-crypto JOSE parser — ring-free but adds a
  second (pure-Rust) crypto impl and more audit surface for no benefit over the standard lib; (c)
  pure Build on aws-lc-rs `ECDSA_P256_SHA256_FIXED` (a direct fit for JOSE's raw r‖s signature) +
  hand-rolled compact-JWS parse — zero new JWT dep, but hand-writes the alg-confusion-sensitive
  critical path, rejected while a mature adopt exists.
- **Isolation**: confined to the new verification module (e.g. `crates/runlet/src/contract.rs`); the
  rest of the box sees only a typed verified-claims struct, never `jsonwebtoken` types.

### Decision: D7 JWKS fetch + cache — Build (thin, on in-tree `reqwest`)

- **Status**: approved
- **Why**: `jsonwebtoken` deliberately does not bundle remote-JWKS, and the fetch/cache/rotation
  logic is small and well-bounded (GET the JWKS JSON, parse the JWK set, cache keys by `kid`, refresh
  on an unknown `kid`, keep last-good on a fetch failure, fail closed when no usable key exists). The
  box **already links `reqwest` 0.13 on the aws-lc-rs TLS provider**, so the fetch adds no new
  transport and no new crypto — building it thin avoids running a **second JWT library** just for a
  key cache (the `jwtk` alternative), matching the box's minimal-dep, adapter-isolated,
  supply-chain-gated posture. Adopting `jwtk` for both concerns was the runner-up (strongest pure
  "Adopt" story) but was declined to avoid two JWT libs and an unconfirmed ES256-on-aws-lc path.
- **Considered**: `jwtk` `RemoteJwksVerifier` (fetch+cache+rotate+verify in one crate) — would also
  own D6, but couples both concerns to a less-ubiquitous crate and would duplicate `jsonwebtoken`;
  `jwt-authorizer` (axum-native wrapper) — brings middleware framing the box's own gate pipeline
  doesn't need.
- **Isolation**: lives behind one JWKS-provider adapter/port alongside the D6 module; supplies
  `DecodingKey`s to the verifier. TLS rides the existing aws-lc-rs provider — no second stack. On a
  JWKS-endpoint outage the adapter serves last-good within a bounded staleness and otherwise rejects
  (never falls back to trusting bare headers).

## Risks / Trade-offs

- **[Hand-rolled JOSE parse (if D6=a) mishandles alg confusion or `alg:none`]** → pin the expected
  `alg` (ES256) before verifying, reject any other `alg` including `none`, and cover with tests for
  forged-alg / stripped-signature tokens.
- **[New crypto/HTTP deps break the `ring`-free invariant]** → gate merges a `cargo tree -i ring`
  check into CI/supply-chain; the decide record must show it empty.
- **[JWKS endpoint outage bricks all enriched traffic]** → cache last-good JWKS; on fetch failure
  keep serving from cache within a bounded staleness; only reject when no usable key exists — but
  still fail closed (reject), never fall back to trusting bare headers.
- **[Contract reuse (`jti`) mistaken for replay]** → explicitly do not treat a repeated `jti` as an
  error; rely on `aud` + `exp` for replay defense, per nexus.
- **[Clock skew rejects valid tokens]** → small configurable leeway on `exp`; document it.
- **[Operators enable the sub-mode without a NetworkPolicy]** → the signature is defense-in-depth,
  not a substitute for origin trust; keep the existing exposed-bind boot guard and document that the
  network path remains the primary boundary.
- **[Two identity paths diverge over time]** → keep sensitive-signal sourcing behind one internal
  seam so both paths funnel through the same gate logic; the sub-mode only swaps the *source*.

## Migration Plan

- Ship dark: `trusted.contract.enabled` defaults **off**; existing deployments are unaffected.
- A nexus-fronted operator sets `jwks_url`/`issuer`/`audience`/`supported_ctr` and flips it on; the
  boot guard refuses an incomplete config.
- Rollback is config-only (disable the sub-mode) — reverts to the current header path.
- The plain-path suspension/scope behavior stays as a fallback, so a bad JWKS rollout can be backed
  out without a redeploy.

## Open Questions

- D6 (verify crate) and D7 (JWKS cache) are **resolved** — see the Decisions above.
- Does the box read `principal_kind`/`permissions` for any authz decision now, or only capture them
  for audit/forwarding this change? (Proposal captures them; gating on `service` vs `user` can be a
  follow-up.)
- Should `supported_ctr` accept a range/min rather than an explicit set, to ease additive bumps?
- Is there a case for forwarding `principal_kind`/`on_behalf_of` downstream (broker `WireInit` /
  box-direct) in this change, or defer with the rest of the cross-repo work?
