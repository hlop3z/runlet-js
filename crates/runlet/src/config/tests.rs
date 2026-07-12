//! The fail-closed exposure gate (`check_exposure`): a non-loopback bind requires either a
//! token or the explicit `allow_unauthenticated` opt-out.

use super::{Config, ServerConfig};
use std::net::{IpAddr, Ipv4Addr};

/// Builds a config with a chosen bind host, token, and opt-out (everything else default).
fn exposure_cfg(host: IpAddr, token: Option<&str>, allow_unauth: bool) -> Config {
    Config {
        server: ServerConfig { host, port: 3000 },
        access_token: token.map(str::to_owned),
        allow_unauthenticated: allow_unauth,
        ..Config::default()
    }
}

/// A loopback bind never needs a token.
#[test]
fn loopback_needs_no_token() {
    let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
    assert!(
        cfg.check_exposure().is_ok(),
        "loopback is fine without a token"
    );
}

/// A non-loopback bind with no token and no opt-out refuses to start.
#[test]
fn exposed_without_token_fails_closed() {
    let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
    assert!(
        cfg.check_exposure().is_err(),
        "0.0.0.0 with no token must refuse to start"
    );
}

/// A token unlocks an exposed bind.
#[test]
fn exposed_with_token_ok() {
    let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
    assert!(
        cfg.check_exposure().is_ok(),
        "a token unlocks an exposed bind"
    );
}

/// The explicit opt-out unlocks an exposed bind (auth terminated upstream).
#[test]
fn exposed_with_explicit_optout_ok() {
    let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), None, true);
    assert!(
        cfg.check_exposure().is_ok(),
        "allow_unauthenticated unlocks an exposed bind"
    );
}

/// The `RUNLET_ACCESS_TOKEN` override sets the token, unlocking an otherwise-exposed bind —
/// the container path where the secret arrives via env, not a baked config file.
#[test]
fn env_token_override_unlocks_exposed_bind() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
    assert!(
        cfg.check_exposure().is_err(),
        "exposed + no token fails first"
    );
    cfg.apply_auth_overrides(Some("from-env".to_owned()), None);
    assert_eq!(cfg.access_token.as_deref(), Some("from-env"));
    assert!(
        cfg.check_exposure().is_ok(),
        "an env-supplied token unlocks the exposed bind"
    );
}

/// A truthy `RUNLET_ALLOW_UNAUTHENTICATED` sets the opt-out; an empty value never does (so a
/// stray/blank env var can't silently unlock the guard).
#[test]
fn env_allow_unauthenticated_override() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
    cfg.apply_auth_overrides(None, Some(""));
    assert!(!cfg.allow_unauthenticated, "blank value stays fail-closed");
    cfg.apply_auth_overrides(None, Some("true"));
    assert!(cfg.allow_unauthenticated, "`true` opts out");
    assert!(cfg.check_exposure().is_ok());
}

/// An empty `RUNLET_ACCESS_TOKEN` is ignored (does not blank out a configured token).
#[test]
fn env_empty_token_is_ignored() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), Some("configured"), false);
    cfg.apply_auth_overrides(Some(String::new()), None);
    assert_eq!(
        cfg.access_token.as_deref(),
        Some("configured"),
        "an empty env token must not clobber the configured one"
    );
}

/// Truthy parsing is case-insensitive + trimmed; everything else (incl. `0`/`false`) is falsey.
#[test]
fn truthy_parsing() {
    for t in ["1", "true", "TRUE", " yes ", "On"] {
        assert!(super::is_truthy(t), "{t:?} should be truthy");
    }
    for f in ["", "0", "false", "no", "off", "maybe"] {
        assert!(!super::is_truthy(f), "{f:?} should be falsey");
    }
}

/// Telemetry defaults to disabled (no endpoint), full sampling, service name `runlet`.
#[test]
fn telemetry_defaults_disabled() {
    let cfg = Config::default();
    assert!(
        cfg.telemetry.otlp_endpoint.is_none(),
        "no endpoint by default ⇒ tracing off, logs only"
    );
    assert!(
        (cfg.telemetry.sample_ratio - 1.0).abs() < f64::EPSILON,
        "default sample ratio is 1.0"
    );
    assert_eq!(cfg.telemetry.service_name, "runlet");
}

/// A `telemetry` block parses its endpoint / ratio / service name; omitted fields default.
#[test]
fn telemetry_block_parses() {
    let json = r#"{"telemetry":{"otlp_endpoint":"http://collector:4317","sample_ratio":0.1}}"#;
    let parsed = serde_json::from_str::<Config>(json);
    assert!(parsed.is_ok(), "telemetry block should parse");
    let cfg = parsed.unwrap_or_default();
    assert_eq!(
        cfg.telemetry.otlp_endpoint.as_deref(),
        Some("http://collector:4317")
    );
    assert!((cfg.telemetry.sample_ratio - 0.1).abs() < f64::EPSILON);
    assert_eq!(
        cfg.telemetry.service_name, "runlet",
        "omitted service_name falls back to the default"
    );
}

/// Events emission is off by default with a sane buffer; a block parses enabled + buffer.
#[test]
fn events_defaults_and_parse() {
    let cfg = Config::default();
    assert!(!cfg.events.enabled, "events off by default (inert)");
    assert_eq!(cfg.events.buffer, 4096);

    let json = r#"{"events":{"enabled":true,"buffer":256}}"#;
    let parsed = serde_json::from_str::<Config>(json);
    assert!(parsed.is_ok(), "events block should parse");
    let parsed_cfg = parsed.unwrap_or_default();
    assert!(parsed_cfg.events.enabled);
    assert_eq!(parsed_cfg.events.buffer, 256);
}

/// Batch caps are present with modest defaults; a block parses overrides.
#[test]
fn batch_defaults_and_parse() {
    let cfg = Config::default();
    assert_eq!(cfg.batch.max_items, 25, "default item cap");
    assert_eq!(cfg.batch.max_input_bytes, 4 * 1024 * 1024);
    assert_eq!(cfg.batch.max_response_bytes, 8 * 1024 * 1024);
    assert_eq!(
        cfg.batch.max_shared_bytes,
        4 * 1024 * 1024,
        "default shared cap"
    );

    let json = r#"{"batch":{"max_items":100,"max_input_bytes":1024,"max_response_bytes":2048,"max_shared_bytes":512}}"#;
    let parsed = serde_json::from_str::<Config>(json);
    assert!(parsed.is_ok(), "batch block should parse");
    let parsed_cfg = parsed.unwrap_or_default();
    assert_eq!(parsed_cfg.batch.max_items, 100);
    assert_eq!(parsed_cfg.batch.max_input_bytes, 1024);
    assert_eq!(parsed_cfg.batch.max_response_bytes, 2048);
    assert_eq!(parsed_cfg.batch.max_shared_bytes, 512);
}

/// `timeout_retryable` defaults to `true` and `retry_after_seconds` to the constant default,
/// both in Rust and through serde (a missing key falls back to the same `Default`); a block
/// parses explicit overrides.
#[test]
fn retry_policy_defaults_and_parse() {
    let cfg = Config::default();
    assert!(cfg.timeout_retryable, "timeout retries by default");
    assert_eq!(cfg.retry_after_seconds, super::DEFAULT_RETRY_AFTER_SECONDS);

    let empty = serde_json::from_str::<Config>("{}").unwrap_or_default();
    assert!(
        empty.timeout_retryable,
        "a missing key defaults to retry, not the type-zero false"
    );
    assert_eq!(
        empty.retry_after_seconds,
        super::DEFAULT_RETRY_AFTER_SECONDS
    );

    let json = r#"{"timeout_retryable":false,"retry_after_seconds":30}"#;
    let parsed = serde_json::from_str::<Config>(json).unwrap_or_default();
    assert!(!parsed.timeout_retryable, "explicit opt-out parses");
    assert_eq!(parsed.retry_after_seconds, 30);
}

/// Builds a config in trusted-header mode with a chosen bind + isolation assertion. A token is
/// set so the base `access_token` guard passes and only the trusted-isolation guard is exercised.
fn trusted_cfg(host: IpAddr, assert_isolation: bool) -> Config {
    let mut cfg = exposure_cfg(host, Some("edge-cred"), false);
    cfg.trusted.enabled = true;
    cfg.trusted.assert_network_isolation = assert_isolation;
    cfg
}

/// Trusted mode on a loopback bind never needs the isolation assertion.
#[test]
fn trusted_loopback_needs_no_isolation() {
    let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), false);
    assert!(
        cfg.check_exposure().is_ok(),
        "loopback trusted mode is fine without asserting isolation"
    );
}

/// Trusted mode on an exposed bind without asserted isolation refuses to start.
#[test]
fn trusted_exposed_without_isolation_fails_closed() {
    let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), false);
    assert!(
        cfg.check_exposure().is_err(),
        "exposed trusted mode must refuse without asserted isolation"
    );
}

/// Asserting isolation unlocks an exposed trusted-mode bind.
#[test]
fn trusted_exposed_with_isolation_ok() {
    let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), true);
    assert!(
        cfg.check_exposure().is_ok(),
        "asserted isolation unlocks an exposed trusted-mode bind"
    );
}

/// Trusted mode disabled leaves an exposed (token-gated) bind unaffected by the isolation guard.
#[test]
fn trusted_disabled_ignores_isolation_guard() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
    cfg.trusted.enabled = false;
    assert!(
        cfg.check_exposure().is_ok(),
        "isolation guard applies only when trusted mode is enabled"
    );
}

/// `debug` (SSRF relaxation) on an exposed bind without asserted isolation refuses to start.
#[test]
fn debug_relaxation_exposed_fails_closed() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
    cfg.debug = true;
    assert!(
        cfg.check_exposure().is_err(),
        "exposed debug (relaxed SSRF) must refuse without asserted isolation"
    );
}

/// A wildcard `*` host allowlist on an exposed bind without asserted isolation refuses to start.
#[test]
fn wildcard_hosts_exposed_fails_closed() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
    cfg.engine.allow_wildcard_hosts = true;
    assert!(
        cfg.check_exposure().is_err(),
        "exposed wildcard host allowlist must refuse without asserted isolation"
    );
}

/// The SSRF relaxation is fine on a loopback bind (local development).
#[test]
fn debug_relaxation_loopback_ok() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
    cfg.debug = true;
    assert!(
        cfg.check_exposure().is_ok(),
        "loopback debug relaxation is fine (local testing)"
    );
}

/// Asserting network isolation unlocks an exposed bind with the SSRF guard relaxed.
#[test]
fn ssrf_relaxation_with_isolation_ok() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
    cfg.debug = true;
    cfg.trusted.assert_network_isolation = true;
    assert!(
        cfg.check_exposure().is_ok(),
        "asserted isolation unlocks an exposed relaxed-SSRF bind"
    );
}

/// A non-relaxed exposed bind (no debug, no wildcard) is unaffected by the SSRF guard.
#[test]
fn no_relaxation_exposed_ok() {
    let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
    assert!(
        cfg.check_exposure().is_ok(),
        "the SSRF guard applies only when a relaxation is active"
    );
}

/// A loopback-only box-direct binding (D8) passes the boot guard.
#[test]
fn local_resource_loopback_ok() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
    drop(cfg.local_resources.insert(
        "pricing".to_owned(),
        super::LocalResource {
            url: "http://127.0.0.1:8080".to_owned(),
        },
    ));
    assert!(
        cfg.check_exposure().is_ok(),
        "a loopback box-direct binding is accepted"
    );
}

/// A remote box-direct binding is refused (a remote logical target must go through a broker).
#[test]
fn local_resource_remote_fails_closed() {
    let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
    drop(cfg.local_resources.insert(
        "pricing".to_owned(),
        super::LocalResource {
            url: "http://93.184.216.34:8080".to_owned(),
        },
    ));
    assert!(
        cfg.check_exposure().is_err(),
        "a remote box-direct binding must refuse to start"
    );
}
