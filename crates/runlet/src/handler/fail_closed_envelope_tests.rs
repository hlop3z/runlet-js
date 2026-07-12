//! The no-broker session error projects to the retryable `EGRESS_UNAVAILABLE` operator fault —
//! the response half of the fail-closed egress invariant (the decision half lives in `broker`).

use super::session_error_envelope;
use crate::broker::SessionError;
use runlet_core::errors::ErrorOwner;

/// An absent/unreachable broker is a retryable operator fault carrying the `EGRESS_UNAVAILABLE`
/// code — the box refuses egress rather than degrading to an ambient path.
#[test]
fn unavailable_maps_to_retryable_egress_unavailable() {
    let envelope = session_error_envelope(SessionError::Unavailable("no broker".to_owned()));
    assert_eq!(envelope.code(), "EGRESS_UNAVAILABLE");
    assert!(envelope.is_retryable());
    assert!(matches!(envelope.owner(), ErrorOwner::Operator));
}
