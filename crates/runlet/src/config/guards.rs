//! Boot-time safety gates and config loading for [`Config`].
//!
//! The fail-closed startup guards (`check_exposure` and its three sub-checks — SSRF relaxation,
//! trusted-header isolation, box-direct local resources) plus the file/env load path. Split from
//! the struct definitions in the parent module so the policy logic reads on its own; the guards
//! operate on the `pub(crate)` fields declared next door.

use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

use super::{Config, is_truthy};

impl Config {
    /// Fail-closed start gate: refuse to bind a **non-loopback** address with no
    /// `access_token` unless the operator explicitly set `allow_unauthenticated` (auth
    /// terminated upstream). A loopback bind is always fine. Keeps a misconfigured
    /// deployment from silently exposing an unauthenticated arbitrary-code executor.
    ///
    /// # Errors
    ///
    /// Returns an error describing the missing gate when the bind is exposed and neither a
    /// token nor the explicit opt-out is present.
    pub(crate) fn check_exposure(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let exposed = !self.server.host.is_loopback();
        if exposed && self.access_token.is_none() && !self.allow_unauthenticated {
            return Err(format!(
                "refusing to start: binding {host} (non-loopback) with no `access_token` and \
                 `allow_unauthenticated` unset. /execute runs caller-supplied code with \
                 caller-supplied credentials, so an unauthenticated reachable port is a full \
                 compromise. Set a token (`access_token` in config, or `RUNLET_ACCESS_TOKEN=<secret>` \
                 in the environment), bind loopback, or opt out with `allow_unauthenticated: true` \
                 (or `RUNLET_ALLOW_UNAUTHENTICATED=1`) if auth is terminated upstream.",
                host = self.server.host,
            )
            .into());
        }
        self.check_trusted_isolation(exposed)?;
        self.check_ssrf_relaxation(exposed)?;
        self.check_local_resources()?;
        Ok(())
    }

    /// Box-direct local-egress boot guard (byo-capabilities D8): every declared binding must target
    /// a loopback/private (co-located) address. A remote target is refused — a remote logical name
    /// must go through a broker, so the box never holds a remote endpoint. Reuses the `http`
    /// capability's classifier ([`runlet_core::check_local_egress_url`]) so one policy governs both
    /// script-controlled `http` targets and operator-declared box-direct bindings.
    ///
    /// # Errors
    ///
    /// Returns an error naming the first binding whose URL is unparseable, non-http(s), or resolves
    /// to any public address.
    #[expect(
        clippy::iter_over_hash_type,
        reason = "boot-guard validation order is irrelevant; every binding is checked and any \
                  failure aborts startup"
    )]
    fn check_local_resources(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        for (name, resource) in &self.local_resources {
            runlet_core::check_local_egress_url(&resource.url).map_err(|err| {
                format!(
                    "refusing to start: box-direct local resource '{name}' -> '{}' is invalid: \
                     {err}. A box-direct binding must be a loopback/private (co-located) http(s) \
                     endpoint; a remote target must go through a broker.",
                    resource.url,
                )
            })?;
        }
        Ok(())
    }

    /// SSRF escape-hatch guard (D5): the private-IP relaxation (`debug`) and the wildcard `*`
    /// host allowlist (`engine.allow_wildcard_hosts`) each collapse the host layer down to the
    /// IP classifier alone, making it the *sole* remaining SSRF defense. Refuse to start with
    /// either active on a non-loopback bind unless network isolation is asserted — mirroring
    /// [`Self::check_trusted_isolation`] and reusing the same operator claim
    /// (`trusted.assert_network_isolation`), so "prod accidentally shipped with the guard
    /// relaxed" is a startup failure rather than a silent exposure. Loopback binds (local dev)
    /// and infra-firewalled deployments that assert isolation are unaffected.
    ///
    /// # Errors
    ///
    /// Returns an error when a relaxation is active, the bind is exposed, and isolation is not
    /// asserted.
    fn check_ssrf_relaxation(&self, exposed: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        let relaxed = self.debug || self.engine.allow_wildcard_hosts;
        if relaxed && exposed && !self.trusted.assert_network_isolation {
            let which = if self.debug {
                "`debug` (relaxes the private-IP SSRF block)"
            } else {
                "`engine.allow_wildcard_hosts` (permits a `*` host allowlist)"
            };
            return Err(format!(
                "refusing to start: {which} is active on {host} (non-loopback) but \
                 `trusted.assert_network_isolation` is unset. That relaxation leaves the IP \
                 classifier as the sole SSRF defense, so the bind must be reachable only through \
                 a trusted edge (enforce with a NetworkPolicy). Bind loopback, drop the \
                 relaxation, or set `trusted.assert_network_isolation: true` once egress is \
                 firewalled at the infra layer.",
                host = self.server.host,
            )
            .into());
        }
        Ok(())
    }

    /// Trusted-mode safety net (D2): trusting `x-*` identity headers rests on the box being
    /// reachable **only** through the edge. Refuse to start in trusted-header mode on a non-loopback
    /// bind unless the operator has asserted network isolation — mirroring the `allow_unauthenticated`
    /// guard, because there is no TLS/JWT check to fall back on once headers are trusted.
    ///
    /// # Errors
    ///
    /// Returns an error when trusted mode is enabled, the bind is exposed, and isolation is not
    /// asserted.
    fn check_trusted_isolation(&self, exposed: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.trusted.enabled && exposed && !self.trusted.assert_network_isolation {
            return Err(format!(
                "refusing to start: trusted-header mode is enabled on {host} (non-loopback) but \
                 `trusted.assert_network_isolation` is unset. The box then trusts `x-*` identity \
                 headers blindly, so it must be reachable only through the edge (enforce with a \
                 NetworkPolicy). Bind loopback, or set `trusted.assert_network_isolation: true` \
                 once the isolation is in place.",
                host = self.server.host,
            )
            .into());
        }
        Ok(())
    }

    /// Loads config from a file path. Returns defaults if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed, or if the
    /// resolved limits violate the parse-headroom invariant (see [`EngineConfig::resolve_limits`]).
    ///
    /// [`EngineConfig::resolve_limits`]: runlet_core::config::EngineConfig::resolve_limits
    pub(crate) fn load(path: &Path) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut config = if path.exists() {
            let contents = fs::read_to_string(path)?;
            serde_json::from_str::<Self>(&contents)?
        } else {
            Self::default()
        };
        config.apply_env_overrides();
        config.engine.resolve_limits()?;
        Ok(config)
    }

    /// Applies environment-variable overrides for the `/execute` auth gate on top of the
    /// file/defaults. A container image never bakes credentials into a config file, so the two
    /// knobs an operator most needs at `docker run` time are injectable via the environment:
    /// `RUNLET_ACCESS_TOKEN` sets the bearer token (a non-empty value wins over the file), and a
    /// truthy `RUNLET_ALLOW_UNAUTHENTICATED` opts a non-loopback bind out of the token requirement
    /// (auth terminated upstream / a local quickstart). Env wins over the file so the same image
    /// can be pointed either way without editing a mounted config. Reads the env once here and
    /// delegates the (unsafe-free, testable) merge to [`Self::apply_auth_overrides`].
    fn apply_env_overrides(&mut self) {
        let token = env::var("RUNLET_ACCESS_TOKEN").ok();
        let allow = env::var("RUNLET_ALLOW_UNAUTHENTICATED").ok();
        self.apply_auth_overrides(token, allow.as_deref());
    }

    /// Pure merge of the auth-gate env overrides, split out so it is unit-testable without
    /// mutating process env (`std::env::set_var` is `unsafe` in edition 2024, which this crate
    /// forbids). A non-empty `access_token` replaces the configured token; a truthy
    /// `allow_unauthenticated` sets the opt-out (it never *clears* a config-set opt-out).
    pub(super) fn apply_auth_overrides(
        &mut self,
        access_token: Option<String>,
        allow_unauthenticated: Option<&str>,
    ) {
        if let Some(token) = access_token
            && !token.is_empty()
        {
            self.access_token = Some(token);
        }
        if allow_unauthenticated.is_some_and(is_truthy) {
            self.allow_unauthenticated = true;
        }
    }
}
