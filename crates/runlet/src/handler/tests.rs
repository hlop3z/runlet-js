//! `/execute` bearer-auth gate: `Authorization` header parsing (constant-time compare itself is
//! tested in `runlet_wire::ct`).

use super::request_authorized;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::header::AUTHORIZATION;

/// A `HeaderMap` carrying a single `Authorization` header value.
fn with_auth(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    drop(headers.insert(AUTHORIZATION, HeaderValue::from_static(value)));
    headers
}

/// A matching bearer token authorizes, case-insensitively on the scheme.
#[test]
fn authorized_accepts_matching_bearer() {
    assert!(
        request_authorized(&with_auth("Bearer s3cret"), "s3cret"),
        "exact match authorizes"
    );
    assert!(
        request_authorized(&with_auth("bearer s3cret"), "s3cret"),
        "lowercase scheme authorizes"
    );
}

/// A wrong, prefix-less, empty, or absent token is rejected.
#[test]
fn authorized_rejects_bad_or_missing() {
    assert!(
        !request_authorized(&with_auth("Bearer wrong"), "s3cret"),
        "wrong token rejected"
    );
    assert!(
        !request_authorized(&with_auth("s3cret"), "s3cret"),
        "missing Bearer prefix rejected"
    );
    assert!(
        !request_authorized(&HeaderMap::new(), "s3cret"),
        "absent header rejected"
    );
    assert!(
        !request_authorized(&with_auth("Bearer "), "s3cret"),
        "empty token rejected"
    );
}
