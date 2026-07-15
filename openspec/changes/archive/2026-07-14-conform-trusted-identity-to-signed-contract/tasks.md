## 1. Build-vs-adopt gate (resolved — see design.md Decisions)

- [x] 1.1 **D6 resolved — Adopt `jsonwebtoken` `default-features=false, features=["aws_lc_rs"]`** (pinned `"10"`; v11 is prerelease). Ring-free, reuses in-tree aws-lc-rs. Verify with `Validation::new(Algorithm::ES256)` (alg pinned) + validated `aud`/`iss`/`exp`; keys built from the JWKS via `DecodingKey::from_jwk`
- [x] 1.2 **D7 resolved — Build a thin JWKS fetch/cache on the in-tree `reqwest`** (aws-lc-rs TLS), behind `ContractVerifier`'s key cache feeding `DecodingKey`s; last-good on outage, fail-closed when no usable key
- [x] 1.3 Added `jsonwebtoken` to Cargo; `cargo vet` succeeds (dep covered by imported audits/exemptions), `cargo deny check licenses bans` ok, and `cargo tree -i ring` proven empty

## 2. Config + boot guard

- [x] 2.1 Added a `trusted.contract` config block (`config/mod.rs` `ContractConfig`): `enabled`, `jwks_url`, `issuer`, `audience`, `supported_ctr` (default `["v1"]`), `leeway_secs`, `min_refresh_secs`; added the `contract` header name (default `x-identity-contract`) and aligned the `plan` header default to `x-workspace-plan`
- [x] 2.2 Added the boot guard (`config/guards.rs` `check_contract_config`): when `trusted.contract.enabled`, refuse to start if `jwks_url`/`issuer`/`audience` are unset; existing exposed-bind guard intact
- [x] 2.3 Boot guard verified end-to-end (Python `test_contract_verification`): an incomplete contract config exits non-zero

## 3. Contract verification module (per the D6/D7 decisions)

- [x] 3.1 New module `crates/runlet/src/contract.rs`: parses a compact JWS via `decode_header`, pins `alg=ES256` (`decode` rejects `alg:none`/other algs before verifying)
- [x] 3.2 JWKS client + cache (`ContractVerifier`): fetches `jwks_url` over the in-tree aws-lc-rs TLS, selects key by `kid`, refreshes on unknown `kid` (min-refresh bound), caches last-good, classifies no-usable-key as `KeysUnavailable`
- [x] 3.3 Verifies the signature against the selected key via `jsonwebtoken`/`aws_lc_rs` (`cargo tree -i ring` empty)
- [x] 3.4 Validates registered claims: `iss` == configured, `aud` == configured pool, `exp` in future (± `leeway_secs`); repeated `jti` is not treated as replay
- [x] 3.5 `ctr` drift gate: rejects a `ctr` outside `supported_ctr` → `CONTRACT_VERSION_UNSUPPORTED`
- [x] 3.6 Typed `Claims` → `VerifiedClaims` projection (`workspace_id`, `sub`, `principal_kind`, `on_behalf_of`, `roles`, `entitlements?`, `suspended?`, `plan?`), absent = `None`; suspended kept tri-state
- [x] 3.7 Unit tests (`contract.rs`): error-code taxonomy is distinct/safe; claim projection preserves the suspended tri-state and carries a resolved profile. **NOTE:** the ES256 signature happy-path + per-claim rejection unit tests (bad-sig / wrong-iss / wrong-aud / expired / `alg:none` / forged-alg / unknown-`kid`-then-refresh) need a live ES256 signer, not available in the build env — deferred to the live-signer E2E (task 6.5); the offline rejection paths are covered by the integration tests (§6.1)

## 4. Wire verification into identity + gates

- [x] 4.1 Extended `TrustedIdentity` with `principal_kind` + `on_behalf_of`; added `apply_contract` overlaying the verified claims (sensitive fields authoritative; `anonymous` cleared)
- [x] 4.2 Request path (`handler/gates.rs` `resolve_identity` → `resolve_identity_contract`): when the sub-mode is on, verifies the contract first; fail-closed reject on any failure; on success overlays claims (claims authoritative, plain `x-runlet-*` mode/capture/log-level still from headers)
- [x] 4.3 Suspension gate: reads `suspended` from the claim tri-state — `Some(true)` ⇒ `SUSPENDED_FORBIDDEN`, `None` ⇒ `SUSPENDED_UNKNOWN` (deny), `Some(false)` ⇒ proceed
- [x] 4.4 Acting-scope: in the sub-mode a verified contract satisfies acting-org assurance (scope tripwire skipped); the plain path keeps `ACTING_SCOPE_REQUIRED` unchanged (existing test `non_trusted_mode_ignores_scope` + `acting_scope_gate_matrix` still green)
- [x] 4.5 Capability authz: unchanged — `has_grant` reads roles/entitlements off `TrustedIdentity`, which the sub-mode now sources from claims (one seam, both paths)
- [x] 4.6 Quota: unchanged — reads `id.plan`, sourced from the `plan` claim in the sub-mode / `x-workspace-plan` header in the plain path; absent ⇒ `most_restrictive`

## 5. Fail-closed + freshness invariants

- [x] 5.1 Enforced: sub-mode on + missing/unverifiable contract ⇒ `403` before egress/execution (verified E2E: `CONTRACT_MISSING`, `CONTRACT_MALFORMED`)
- [x] 5.2 Freshness: `verify` runs per request and never caches a verified contract's claims (only public keys are cached), so nothing is reused past `exp`; `jsonwebtoken` re-checks `exp` each call
- [x] 5.3 Distinct `denied` audit reasons emitted for each rejection (`CONTRACT_MISSING`/`_MALFORMED`/`_INVALID`/`_UNKNOWN_KEY`/`_KEYS_UNAVAILABLE`/`_VERSION_UNSUPPORTED`, `SUSPENDED_UNKNOWN`) via the existing `emit_denied`

## 6. Tests, docs, supply chain

- [x] 6.1 Integration coverage (`tests/test_simple.py` `test_contract_verification`): sub-mode on ⇒ missing contract `403 CONTRACT_MISSING` (scope header no longer suffices), malformed contract `403 CONTRACT_MALFORMED`, and the boot guard; sub-mode-off unchanged (existing trusted-pipeline tests still pass). Verified in-container end-to-end against a live box
- [x] 6.2 Updated `docs/design/multitenant-trust.md`: refreshed the stale header table (bare headers are plain-path only; plan renamed), corrected the "no crypto in the box" absolutes, and added the "Signed-contract sub-mode (opt-in)" section + pipeline diagram
- [x] 6.3 `cargo tree -i ring` empty; `cargo vet` + `cargo deny check licenses bans` clean; `cargo clippy -p runlet` and `cargo fmt --all --check` clean; `cargo test -p runlet` 105 passed
- [x] 6.4 Out-of-scope follow-ups noted in `proposal.md` (Impact) and the sub-mode doc section: event-logs isolation + `X-Runlet-Tenant` route; canonical cross-repo vocabulary doc
- [ ] 6.5 **Follow-up (needs a live ES256 signer + JWKS):** end-to-end happy-path — a validly signed contract executes; `aud`-for-another-box / expired / unknown-`ctr` / bad-signature each reject; unknown-`kid` triggers a JWKS refresh then verifies. Add when the harness gains an EC signer (or a nexus test double)
