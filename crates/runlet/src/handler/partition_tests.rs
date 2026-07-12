//! The fairness/cache key source: caller-asserted in single-tenant mode, the trusted tenant id
//! (ignoring any caller-asserted value) in trusted mode.

use super::resolve_partition;
use crate::identity::TrustedIdentity;

/// Without a trusted identity the caller-asserted value is used (single-tenant behavior).
#[test]
fn single_tenant_uses_caller_asserted() {
    let key = resolve_partition(None, Some("caller-key".to_owned()));
    assert_eq!(key.as_deref(), Some("caller-key"));
}

/// In trusted mode the key is the trusted tenant id and the caller-asserted value is ignored.
#[test]
fn trusted_uses_tenant_and_ignores_caller() {
    let identity = TrustedIdentity {
        tenant: Some("ws_acme".to_owned()),
        ..TrustedIdentity::default()
    };
    let key = resolve_partition(Some(&identity), Some("spoofed-partition".to_owned()));
    assert_eq!(
        key.as_deref(),
        Some("ws_acme"),
        "trusted tenant wins; caller-asserted partition is ignored"
    );
}
