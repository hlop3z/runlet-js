//! Host-allowlist wildcard gating, the explicit scheme allowlist, and the alt-encoding
//! canonicalization the guard inherits from the `url` crate (pinned by our own tests).

use super::{is_host_allowed, is_local_bypass, is_scheme_allowed, validate_url};
use crate::ssrf::block_private_ip;

/// Builds a single-element allow list.
fn allow(host: &str) -> Vec<String> {
    vec![host.to_owned()]
}

/// `*` matches every host only when `wildcard_allowed`; otherwise it's an inert literal.
#[test]
fn wildcard_honored_only_when_allowed() {
    assert!(
        is_host_allowed("evil.example", 443, &allow("*"), true),
        "wildcard matches when allowed"
    );
    assert!(
        !is_host_allowed("evil.example", 443, &allow("*"), false),
        "wildcard is inert when not allowed"
    );
}

/// An explicit host matches case-insensitively; an unlisted host never matches.
#[test]
fn explicit_hosts_match_case_insensitively() {
    assert!(
        is_host_allowed("API.Example.com", 443, &allow("api.example.com"), false),
        "case-insensitive exact match"
    );
    assert!(
        !is_host_allowed("other.example", 443, &allow("api.example.com"), false),
        "unlisted host rejected"
    );
}

/// D6: a `host:port` allowlist entry matches only that exact target; a bare host matches any
/// port; and only the `host:port` form grants the private-IP bypass.
#[test]
fn hostport_allowlist_is_port_specific() {
    let listed = allow("localhost:8000");
    assert!(
        is_host_allowed("localhost", 8000, &listed, false),
        "the exact host:port is allowed"
    );
    assert!(
        !is_host_allowed("localhost", 9999, &listed, false),
        "a different port on the same host is not allowed by a host:port entry"
    );
    assert!(
        is_local_bypass("localhost", 8000, &listed),
        "the exact host:port grants the local bypass"
    );
    assert!(
        !is_local_bypass("localhost", 9999, &listed),
        "a different port does not grant the bypass"
    );
    assert!(
        !is_local_bypass("localhost", 8000, &allow("localhost")),
        "a bare host entry never grants the bypass"
    );
}

/// D6 end-to-end pre-check: an allowlisted `localhost:8000` target is permitted with `debug`
/// off, while an un-named local port is still blocked by the private-IP guard.
#[test]
fn named_local_target_bypasses_private_block() {
    let listed = allow("localhost:8000");
    assert!(
        validate_url("http://localhost:8000/health", &listed, false, false).is_ok(),
        "the named local target is reachable with debug off"
    );
    assert!(
        validate_url("http://localhost:9999/health", &listed, false, false).is_err(),
        "an un-named local port is still blocked by the private-IP guard"
    );
}

/// Only `http`/`https` pass the scheme allowlist; every other scheme is rejected.
#[test]
fn scheme_allowlist_permits_only_http() {
    assert!(is_scheme_allowed("http"), "http is allowed");
    assert!(is_scheme_allowed("https"), "https is allowed");
    for scheme in ["file", "gopher", "ftp", "data", "ws"] {
        assert!(!is_scheme_allowed(scheme), "{scheme} is rejected");
    }
}

/// `validate_url` rejects a non-http(s) URL before any host/IP check — the scheme gate
/// is deterministic and does not depend on the client's supported schemes.
#[test]
fn validate_url_rejects_non_http_scheme() {
    assert!(
        validate_url("file:///etc/passwd", &allow("*"), true, true).is_err(),
        "file:// is refused up front"
    );
    assert!(
        validate_url("gopher://evil.example/", &allow("evil.example"), true, true).is_err(),
        "gopher:// is refused up front"
    );
}

/// D4: decimal/octal/hex/short-form IP literals are canonicalized to a dotted quad by the
/// `url` crate's WHATWG host parser *before* classification, so an internal address written
/// in an alternate encoding is blocked exactly as the dotted-quad form is. Pinned here so a
/// future `url`-crate behavior change is caught by our suite, not silently reopened. Uses the
/// same parse path `validate_url` runs (`reqwest::Url::parse` → `host_str` → `block_private_ip`).
#[test]
fn alt_encoded_loopback_canonicalizes_to_blocked() {
    for url in [
        "http://2130706433/", // decimal 127.0.0.1
        "http://0x7f000001/", // hex 127.0.0.1
        "http://127.1/",      // short form 127.0.0.1
        "http://0177.0.0.1/", // octal-first-octet 127.0.0.1
    ] {
        let parsed = reqwest::Url::parse(url)
            .unwrap_or_else(|_err| unreachable!("test URL must parse: {url}"));
        let host = parsed
            .host_str()
            .unwrap_or_else(|| unreachable!("test URL must have a host: {url}"));
        assert_eq!(
            host, "127.0.0.1",
            "{url} canonicalizes to dotted-quad loopback"
        );
        let port = parsed.port_or_known_default().unwrap_or(80);
        assert!(
            block_private_ip(host, port, false).is_err(),
            "{url} resolves to a blocked loopback address"
        );
    }
}
