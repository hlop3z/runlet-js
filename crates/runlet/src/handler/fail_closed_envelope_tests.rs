//! The no-sidecar session error projects to the retryable `EGRESS_UNAVAILABLE` operator fault —
//! the response half of the fail-closed egress invariant (the decision half lives in `sidecar`).

use super::session_error_envelope;
use crate::sidecar::SessionError;
use runlet_core::errors::ErrorOwner;

/// An absent/unreachable sidecar is a retryable operator fault carrying the `EGRESS_UNAVAILABLE`
/// code — the box refuses egress rather than degrading to an ambient path.
#[test]
fn unavailable_maps_to_retryable_egress_unavailable() {
    let envelope = session_error_envelope(SessionError::Unavailable("no sidecar".to_owned()));
    assert_eq!(envelope.code(), "EGRESS_UNAVAILABLE");
    assert!(envelope.is_retryable());
    assert!(matches!(envelope.owner(), ErrorOwner::Operator));
}
