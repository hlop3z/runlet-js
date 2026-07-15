//! Signed-identity-contract verification (the `trusted.contract` sub-mode).
//!
//! When the operator enables the sub-mode, the box verifies nexus's `x-identity-contract` — a
//! compact **ES256 JWS** — and sources the revocation-sensitive identity (roles, entitlements,
//! suspended, plan) plus `principal_kind`/`on_behalf_of` from the **verified claims** instead of the
//! bare headers nexus retired. Verification is the whole security surface, so it lives behind this
//! one adapter: the rest of the box only ever sees a [`VerifiedClaims`], never a `jsonwebtoken` type.
//!
//! **Verify-only** — the box holds no signing key. It fetches the public JWKS from the configured
//! endpoint, caches keys by `kid`, refreshes on an unknown `kid` (bounded by `min_refresh`), and
//! keeps the last-good key set across a transient JWKS outage. Signature verification reuses the
//! process-wide aws-lc-rs provider (the `aws_lc_rs` `jsonwebtoken` backend), so no second crypto
//! stack is linked. See `docs/design/multitenant-trust.md` and the change's `design.md` (D6/D7).

use std::collections::{HashMap, HashSet};
use std::slice::from_ref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::config::ContractConfig;

/// Why a contract failed verification. Each maps to a fail-closed rejection with a distinct audit
/// reason code; none of them ever lets an unverified request proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContractError {
    /// The header could not be decoded, carried no `kid`, or used a disallowed `alg`.
    Malformed,
    /// The `kid` is absent from the JWKS even after a refresh (a key rotated out, or a foreign key).
    UnknownKey,
    /// No verifying key is available at all (cold JWKS fetch failed and the cache is empty).
    KeysUnavailable,
    /// The signature, `iss`, `aud`, or `exp` failed validation.
    Invalid,
    /// The `ctr` contract-version claim is not in this box's supported set (the drift tripwire).
    UnsupportedVersion,
}

impl ContractError {
    /// The stable audit reason code for this failure (surfaced in the `denied` audit event).
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Malformed => "CONTRACT_MALFORMED",
            Self::UnknownKey => "CONTRACT_UNKNOWN_KEY",
            Self::KeysUnavailable => "CONTRACT_KEYS_UNAVAILABLE",
            Self::Invalid => "CONTRACT_INVALID",
            Self::UnsupportedVersion => "CONTRACT_VERSION_UNSUPPORTED",
        }
    }

    /// A caller-safe message (no key material, no crypto internals) for the rejection envelope.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Malformed => "identity contract is malformed",
            Self::UnknownKey => "identity contract signed by an unknown key",
            Self::KeysUnavailable => "identity verification keys are unavailable",
            Self::Invalid => "identity contract failed verification",
            Self::UnsupportedVersion => "identity contract version is not supported",
        }
    }
}

/// The identity carried by a verified contract, projected into the box's vocabulary. `tenant` is the
/// `workspace_id` claim; `user` is `sub`. `suspended` stays **tri-state** — `None` means nexus had
/// no resolved profile, which the gate treats as *unknown → deny*, never as not-suspended.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedClaims {
    /// The acting workspace id (`workspace_id` claim) — the box's tenant.
    pub(crate) tenant: Option<String>,
    /// The subject (`sub` claim) — the user/key id, for audit.
    pub(crate) user: Option<String>,
    /// The principal kind (`user`/`apikey`/`service`), nexus-authored.
    pub(crate) principal_kind: Option<String>,
    /// The acted-for subject (`on_behalf_of`) — present only for an api-key principal.
    pub(crate) on_behalf_of: Option<String>,
    /// Coarse global roles the caller holds.
    pub(crate) roles: Vec<String>,
    /// Entitlements the caller holds (an absent claim flattens to empty — the coarse authz gate then
    /// fails closed on any gated capability, which is loud, not dangerous).
    pub(crate) entitlements: Vec<String>,
    /// Suspension, tri-state: `Some(true)` blocks; `Some(false)` proceeds; `None` is unknown → deny.
    pub(crate) suspended: Option<bool>,
    /// The acting workspace's plan tier (absent ⇒ not-provisioned).
    pub(crate) plan: Option<String>,
}

/// The subset of contract claims the box reads. `exp`/`aud`/`iss` are validated by `jsonwebtoken`
/// (not stored here); `ctr` is checked against the supported set after signature verification.
#[derive(Debug, Deserialize)]
struct Claims {
    /// Subject (user id / key id).
    #[serde(default)]
    sub: Option<String>,
    /// The acting workspace id.
    #[serde(default)]
    workspace_id: Option<String>,
    /// Principal kind (`user`/`apikey`/`service`).
    #[serde(default)]
    principal_kind: Option<String>,
    /// The creating user an api-key acts for.
    #[serde(default)]
    on_behalf_of: Option<String>,
    /// Coarse global roles.
    #[serde(default)]
    roles: Vec<String>,
    /// Global entitlements; omitted when nexus has no resolved profile.
    #[serde(default)]
    entitlements: Option<Vec<String>>,
    /// Suspension; omitted when nexus has no resolved profile (absence ⇒ unknown).
    #[serde(default)]
    suspended: Option<bool>,
    /// Acting workspace plan tier; omitted when unresolved.
    #[serde(default)]
    plan: Option<String>,
    /// Contract-shape version — the drift gate.
    ctr: String,
}

impl Claims {
    /// Projects the raw claims into the box's [`VerifiedClaims`] vocabulary.
    fn into_verified(self) -> VerifiedClaims {
        VerifiedClaims {
            tenant: self.workspace_id,
            user: self.sub,
            principal_kind: self.principal_kind,
            on_behalf_of: self.on_behalf_of,
            roles: self.roles,
            entitlements: self.entitlements.unwrap_or_default(),
            suspended: self.suspended,
            plan: self.plan,
        }
    }
}

/// The `kid`-keyed verifying-key cache plus the last refresh instant (for the min-refresh bound).
#[derive(Debug, Default)]
struct KeyCache {
    /// Verifying keys by `kid`.
    keys: HashMap<String, Arc<DecodingKey>>,
    /// When the cache was last refreshed (`None` ⇒ never fetched).
    last_refresh: Option<Instant>,
}

/// Verifies `x-identity-contract` tokens against a cached, rotating JWKS. Built once at startup when
/// the sub-mode is enabled and shared (behind `Arc`) into the request path.
#[derive(Debug)]
pub(crate) struct ContractVerifier {
    /// HTTP client for JWKS fetches (reuses the process rustls/aws-lc-rs stack).
    client: Client,
    /// The JWKS endpoint.
    jwks_url: String,
    /// The `ctr` values this box accepts.
    supported_ctr: HashSet<String>,
    /// Minimum interval between unknown-`kid` refetches, bounding churn under bad-token bursts.
    min_refresh: Duration,
    /// Pre-built ES256 validation (alg pinned, `iss`/`aud`/`exp` required + checked, leeway applied).
    validation: Validation,
    /// The rotating key cache.
    cache: RwLock<KeyCache>,
}

impl ContractVerifier {
    /// Builds a verifier from the `trusted.contract` config and a shared HTTP client. The JWKS is
    /// fetched lazily on the first verification (boot never blocks on the identity plane).
    pub(crate) fn new(config: &ContractConfig, client: Client) -> Self {
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_issuer(from_ref(&config.issuer));
        validation.set_audience(from_ref(&config.audience));
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        validation.leeway = config.leeway_secs;
        Self {
            client,
            jwks_url: config.jwks_url.clone(),
            supported_ctr: config.supported_ctr.iter().cloned().collect(),
            min_refresh: Duration::from_secs(config.min_refresh_secs),
            validation,
            cache: RwLock::new(KeyCache::default()),
        }
    }

    /// Verifies a compact `x-identity-contract` token end to end: `alg`-pinned signature check against
    /// the `kid`'s key, then `iss`/`aud`/`exp` and the `ctr` version gate.
    ///
    /// # Errors
    ///
    /// Returns a [`ContractError`] for any failure — the caller rejects fail-closed on every one.
    pub(crate) async fn verify(&self, token: &str) -> Result<VerifiedClaims, ContractError> {
        let header = decode_header(token).map_err(|_err| ContractError::Malformed)?;
        let kid = header.kid.ok_or(ContractError::Malformed)?;
        let key = self.key_for_kid(&kid).await?;
        // `decode` re-checks that the header `alg` is ES256 (rejecting alg-confusion and `alg:none`)
        // and validates `iss`/`aud`/`exp` before returning the claims.
        let data = decode::<Claims>(token, &key, &self.validation)
            .map_err(|_err| ContractError::Invalid)?;
        if !self.supported_ctr.contains(&data.claims.ctr) {
            return Err(ContractError::UnsupportedVersion);
        }
        Ok(data.claims.into_verified())
    }

    /// Resolves the verifying key for `kid`: the cached key, or a single bounded refresh then retry.
    async fn key_for_kid(&self, kid: &str) -> Result<Arc<DecodingKey>, ContractError> {
        if let Some(key) = self.cached_key(kid).await {
            return Ok(key);
        }
        self.refresh_keys().await;
        if let Some(key) = self.cached_key(kid).await {
            return Ok(key);
        }
        // Distinguish "we have keys but not this one" (rotated-out / foreign key) from "we have no
        // usable keys at all" (the JWKS endpoint is down and the cache is cold) for the audit trail.
        if self.cache_is_empty().await {
            Err(ContractError::KeysUnavailable)
        } else {
            Err(ContractError::UnknownKey)
        }
    }

    /// Reads a cached key by `kid`, if present.
    async fn cached_key(&self, kid: &str) -> Option<Arc<DecodingKey>> {
        self.cache.read().await.keys.get(kid).map(Arc::clone)
    }

    /// Whether the key cache holds no keys (used to classify an unresolved `kid`).
    async fn cache_is_empty(&self) -> bool {
        self.cache.read().await.keys.is_empty()
    }

    /// Refetches the JWKS, subject to the min-refresh bound. On success replaces the key set; on a
    /// fetch/parse failure keeps the last-good keys (only the timestamp advances, so a burst of
    /// unverifiable tokens cannot hammer the endpoint). Best-effort — errors are logged, not raised.
    async fn refresh_keys(&self) {
        {
            let cache = self.cache.read().await;
            if let Some(last) = cache.last_refresh
                && last.elapsed() < self.min_refresh
            {
                return;
            }
        }
        let fetched = self.fetch_keys().await;
        let failed = fetched.is_none();
        {
            let mut cache = self.cache.write().await;
            cache.last_refresh = Some(Instant::now());
            if let Some(keys) = fetched {
                cache.keys = keys;
            }
        }
        if failed {
            tracing::warn!(
                jwks_url = self.jwks_url.as_str(),
                "JWKS refresh failed; serving last-good keys (contract verification fails closed)"
            );
        }
    }

    /// Fetches and parses the JWKS into a `kid`-keyed map of verifying keys. Returns `None` on any
    /// transport/parse error (the caller keeps the last-good set); keys without a `kid` or that fail
    /// to build are skipped.
    async fn fetch_keys(&self) -> Option<HashMap<String, Arc<DecodingKey>>> {
        let response = self.client.get(&self.jwks_url).send().await.ok()?;
        let set = response.json::<JwkSet>().await.ok()?;
        let mut keys = HashMap::new();
        for jwk in &set.keys {
            if let Some(kid) = jwk.common.key_id.clone()
                && let Ok(key) = DecodingKey::from_jwk(jwk)
            {
                drop(keys.insert(kid, Arc::new(key)));
            }
        }
        Some(keys)
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the pure pieces (error taxonomy, claim projection, ctr gate). Signature/
    //! JWKS behavior is exercised end to end by the integration suite (a live signer + JWKS server).

    use super::{Claims, ContractError};

    /// Every error maps to a distinct, stable audit code and a non-empty caller-safe message.
    #[test]
    fn error_codes_are_distinct_and_safe() {
        let all = [
            ContractError::Malformed,
            ContractError::UnknownKey,
            ContractError::KeysUnavailable,
            ContractError::Invalid,
            ContractError::UnsupportedVersion,
        ];
        for err in all {
            assert!(err.code().starts_with("CONTRACT_"), "stable code prefix");
            assert!(!err.message().is_empty(), "has a caller-safe message");
        }
        // Codes are unique across the taxonomy.
        let mut codes: Vec<&str> = all.iter().map(|err| err.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len(), "each error has a unique code");
    }

    /// An absent `entitlements` claim flattens to empty (fail-closed authz), while `suspended` keeps
    /// its tri-state `None` (unknown → the gate denies).
    #[test]
    fn projection_preserves_suspended_tristate() {
        let json = r#"{"workspace_id":"ws_a","sub":"u_1","roles":["admin"],"ctr":"v1"}"#;
        let claims: Claims = serde_json::from_str(json).expect("claims parse");
        let verified = claims.into_verified();
        assert_eq!(verified.tenant.as_deref(), Some("ws_a"));
        assert_eq!(verified.user.as_deref(), Some("u_1"));
        assert_eq!(verified.roles, vec!["admin".to_owned()]);
        assert!(
            verified.entitlements.is_empty(),
            "absent entitlements ⇒ empty"
        );
        assert_eq!(verified.suspended, None, "absent suspended stays unknown");
        assert_eq!(verified.plan, None, "absent plan stays not-provisioned");
    }

    /// A resolved profile round-trips its suspended/entitlements/plan values.
    #[test]
    fn projection_carries_resolved_profile() {
        let json = r#"{"workspace_id":"ws_a","sub":"u_1","principal_kind":"apikey",
            "on_behalf_of":"u_human","entitlements":["db"],"suspended":false,"plan":"pro","ctr":"v1"}"#;
        let claims: Claims = serde_json::from_str(json).expect("claims parse");
        let verified = claims.into_verified();
        assert_eq!(verified.principal_kind.as_deref(), Some("apikey"));
        assert_eq!(verified.on_behalf_of.as_deref(), Some("u_human"));
        assert_eq!(verified.entitlements, vec!["db".to_owned()]);
        assert_eq!(verified.suspended, Some(false));
        assert_eq!(verified.plan.as_deref(), Some("pro"));
    }
}
