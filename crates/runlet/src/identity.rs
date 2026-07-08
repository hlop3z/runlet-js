//! The trusted-identity request contract (section 1.3 / 2 of the multitenant-trust change).
//!
//! In trusted-header mode the edge (nexus) authenticates the caller, strips any client-supplied
//! `x-*`, and injects a trusted identity. The box derives tenant + user identity **solely** from
//! the operator-configured header names — nothing the executing script or the raw client can assert
//! reaches this. See `docs/design/multitenant-trust.md`.

use axum::http::HeaderMap;
use runlet_core::LogLevel;

use crate::config::TrustedHeaders;

/// The gateway-asserted execution mode (OQ1, Stripe's isolation model). A **live** run streams its
/// logs to the tenant stream; a **test/playground** run is response-mirror-only and never enters the
/// live stream / billing / audit. Both are orthogonal to the diagnostic-capture flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Production traffic — logs stream to the tenant.
    #[default]
    Live,
    /// Test / playground traffic — response-mirror-only, never streams.
    Test,
}

/// The trusted identity extracted from the edge-injected headers for one request.
///
/// Every field comes only from a configured trusted header; a client-set body/identity value never
/// populates it. `tenant` is opaque (an already-authorized acting-workspace id); `roles`/
/// `entitlements` drive the coarse member-capability gate; `suspended`/`anonymous` are hard-reject
/// signals; `plan` selects the quota tier.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrustedIdentity {
    /// Trusted tenant / acting-workspace id (the universal key), if present.
    pub(crate) tenant: Option<String>,
    /// Trusted user id, for audit/logging, if present.
    pub(crate) user: Option<String>,
    /// Trusted roles the caller holds.
    pub(crate) roles: Vec<String>,
    /// Trusted entitlements the caller holds.
    pub(crate) entitlements: Vec<String>,
    /// The principal is suspended — a hard reject.
    pub(crate) suspended: bool,
    /// The caller is anonymous — a hard reject.
    pub(crate) anonymous: bool,
    /// The tenant's plan (quota tier), if present.
    pub(crate) plan: Option<String>,
    /// The acting-org assurance the edge asserts per request (nexus N5). Populated from the
    /// configured scope header; the gate requires `Some("acting")` for tenant-scoped work.
    pub(crate) scope: Option<String>,
    /// Gateway-asserted execution mode (OQ1). Governs diagnostic-log routing only in this change: a
    /// `Test` run is response-mirror-only and never streams. Defaults to `Live`.
    pub(crate) mode: RunMode,
    /// Gateway-asserted request for the response-mirror `logs` list (D5). `false` unless the trusted
    /// capture header is truthy.
    pub(crate) capture: bool,
    /// Gateway-supplied per-request log-level-floor override (OQ2). `None` unless a valid level name
    /// is present in the trusted log-level header.
    pub(crate) log_level: Option<LogLevel>,
}

impl TrustedIdentity {
    /// Extracts the trusted identity from `headers`, reading **only** the configured header names.
    /// Missing headers leave their fields empty/`false`; malformed (non-UTF-8) header values are
    /// ignored (treated as absent).
    pub(crate) fn from_headers(headers: &HeaderMap, names: &TrustedHeaders) -> Self {
        Self {
            tenant: header_value(headers, &names.tenant),
            user: header_value(headers, &names.user),
            roles: header_list(headers, &names.roles),
            entitlements: header_list(headers, &names.entitlements),
            suspended: header_flag(headers, &names.suspended),
            anonymous: header_flag(headers, &names.anonymous),
            plan: header_value(headers, &names.plan),
            scope: header_value(headers, &names.scope),
            mode: header_mode(headers, &names.mode),
            capture: header_flag(headers, &names.capture),
            log_level: header_value(headers, &names.log_level)
                .and_then(|raw| LogLevel::parse(&raw.to_ascii_lowercase())),
        }
    }

    /// `true` if the caller holds `needle` as either a role or an entitlement (the coarse gate
    /// treats the two sets as one membership pool).
    pub(crate) fn has_grant(&self, needle: &str) -> bool {
        self.entitlements.iter().any(|item| item == needle)
            || self.roles.iter().any(|item| item == needle)
    }
}

/// Reads a single trusted header as a trimmed, non-empty owned string (`None` if absent, non-UTF-8,
/// or blank).
fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Reads a comma-separated trusted header into a de-blanked list of trimmed values (empty if absent).
fn header_list(headers: &HeaderMap, name: &str) -> Vec<String> {
    header_value(headers, name).map_or_else(Vec::new, |raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

/// Reads the execution-mode trusted header. `test`/`playground` (case-insensitive) ⇒ [`RunMode::Test`];
/// anything else, including absent or `live`, ⇒ [`RunMode::Live`] (the safe default — a run only
/// escapes the live stream when the edge *explicitly* marks it test).
fn header_mode(headers: &HeaderMap, name: &str) -> RunMode {
    match header_value(headers, name).map(|value| value.to_ascii_lowercase()) {
        Some(value) if value == "test" || value == "playground" => RunMode::Test,
        _other => RunMode::Live,
    }
}

/// Reads a boolean trusted header — `true` only for a case-insensitive `"true"` or `"1"` (anything
/// else, including absent, is `false`).
fn header_flag(headers: &HeaderMap, name: &str) -> bool {
    header_value(headers, name).is_some_and(|value| {
        let lowered = value.to_ascii_lowercase();
        lowered == "true" || lowered == "1"
    })
}

#[cfg(test)]
mod tests {
    //! Header extraction: only configured names are read, flags/lists parse, and client-set values
    //! under non-trusted names have no effect.

    use super::TrustedIdentity;
    use crate::config::TrustedHeaders;
    use axum::http::{HeaderMap, HeaderValue};

    /// Builds a `HeaderMap` from `(name, value)` pairs.
    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            drop(map.insert(*name, HeaderValue::from_static(value)));
        }
        map
    }

    /// The tenant/user/roles/entitlements/flags/plan all parse from their default header names.
    #[test]
    fn extracts_all_default_headers() {
        let map = headers(&[
            ("x-workspace-id", "ws_acme"),
            ("x-user-id", "u_42"),
            ("x-user-roles", "admin, billing"),
            ("x-user-entitlements", "db, mail"),
            ("x-user-suspended", "false"),
            ("x-auth-anonymous", "0"),
            ("x-tenant-plan", "pro"),
            ("x-tenant-scope", "acting"),
        ]);
        let id = TrustedIdentity::from_headers(&map, &TrustedHeaders::default());
        assert_eq!(id.tenant.as_deref(), Some("ws_acme"));
        assert_eq!(id.user.as_deref(), Some("u_42"));
        assert_eq!(id.roles, vec!["admin".to_owned(), "billing".to_owned()]);
        assert_eq!(id.entitlements, vec!["db".to_owned(), "mail".to_owned()]);
        assert!(!id.suspended, "'false' is not suspended");
        assert!(!id.anonymous, "'0' is not anonymous");
        assert_eq!(id.plan.as_deref(), Some("pro"));
        assert_eq!(id.scope.as_deref(), Some("acting"));
        assert!(id.has_grant("db"), "entitlement grant matches");
        assert!(id.has_grant("admin"), "role grant matches");
        assert!(!id.has_grant("mongo"), "absent grant does not match");
    }

    /// The trusted `mode` + `capture` + per-request log-level override parse from their default
    /// header names; absent mode is `live`, absent capture is `false` (§3.1 / OQ5).
    #[test]
    fn resolves_mode_capture_and_log_level() {
        use super::RunMode;
        use runlet_core::LogLevel;

        let map = headers(&[
            ("x-tenant-scope", "acting"),
            ("x-runlet-mode", "test"),
            ("x-runlet-capture", "true"),
            ("x-runlet-log-level", "DEBUG"),
        ]);
        let id = TrustedIdentity::from_headers(&map, &TrustedHeaders::default());
        assert_eq!(id.mode, RunMode::Test, "test mode parses");
        assert!(id.capture, "capture flag parses");
        assert_eq!(
            id.log_level,
            Some(LogLevel::Debug),
            "the per-request floor override parses case-insensitively"
        );

        // Absent signals default to (live, no-capture, no override).
        let bare = headers(&[("x-tenant-scope", "acting")]);
        let id = TrustedIdentity::from_headers(&bare, &TrustedHeaders::default());
        assert_eq!(id.mode, RunMode::Live, "absent mode defaults to live");
        assert!(!id.capture, "absent capture defaults to false");
        assert_eq!(id.log_level, None, "absent override is None");
    }

    /// The suspended / anonymous flags read `true`/`1` case-insensitively.
    #[test]
    fn flags_are_truthy_only_for_true_or_one() {
        let suspended = headers(&[("x-user-suspended", "TRUE")]);
        assert!(TrustedIdentity::from_headers(&suspended, &TrustedHeaders::default()).suspended);
        let anon = headers(&[("x-auth-anonymous", "1")]);
        assert!(TrustedIdentity::from_headers(&anon, &TrustedHeaders::default()).anonymous);
        let neither = headers(&[("x-user-suspended", "yes")]);
        assert!(!TrustedIdentity::from_headers(&neither, &TrustedHeaders::default()).suspended);
    }

    /// A client-set value under a non-configured name is ignored; only the configured header counts.
    #[test]
    fn client_supplied_identity_is_ignored() {
        // Configure a custom tenant header; the default `x-workspace-id` a client might set is not read.
        let names = TrustedHeaders {
            tenant: "x-trusted-tenant".to_owned(),
            ..TrustedHeaders::default()
        };
        let map = headers(&[
            ("x-workspace-id", "spoofed"),
            ("x-trusted-tenant", "ws_real"),
        ]);
        let id = TrustedIdentity::from_headers(&map, &names);
        assert_eq!(
            id.tenant.as_deref(),
            Some("ws_real"),
            "only the configured trusted header is read"
        );
    }

    /// The acting-org scope is read only from the configured scope header name; a value under the
    /// default name has no effect once the operator renamed it.
    #[test]
    fn scope_reads_only_configured_name() {
        let names = TrustedHeaders {
            scope: "x-acting-scope".to_owned(),
            ..TrustedHeaders::default()
        };
        let map = headers(&[("x-tenant-scope", "acting"), ("x-acting-scope", "acting")]);
        let id = TrustedIdentity::from_headers(&map, &names);
        assert_eq!(
            id.scope.as_deref(),
            Some("acting"),
            "the configured scope header is read"
        );
        // The default name is now inert.
        let only_default = headers(&[("x-tenant-scope", "acting")]);
        let id = TrustedIdentity::from_headers(&only_default, &names);
        assert_eq!(
            id.scope, None,
            "the default scope name is not read once overridden"
        );
    }
}
