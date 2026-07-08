//! HTTP status projection — the single source of truth (design D1/D2/D6) that turns a
//! classified system-error envelope `(retryable, owner, code)` into its HTTP status line plus
//! whether a `Retry-After` header rides with it.
//!
//! The invariant: the status **class** is a pure function of `(success, retryable)` —
//! `retryable = true ⇒ 5xx` (retry), `retryable = false ⇒ 4xx` (park / dead-letter). `owner`
//! and `code` only pick *which* code inside the class; they never move the class. `429` is
//! **never** produced — its `4xx` digit would make a status-line worker park a retryable
//! response, the exact failure this projection exists to kill.
//!
//! Every classified fault (engine error, capacity/quota rejection, egress-session failure,
//! oversize input) flows through [`project_envelope`]; nothing hand-rolls a status. Access-control
//! denials (`401`/`403`) are not retry-classified faults and keep their standard HTTP semantics at
//! the call site rather than routing through here.

use runlet_core::errors::{ErrorEnvelope, ErrorOwner};

/// Box-internal code within the retryable class — the one retryable that maps to `500` (a bug in
/// the box), while every other transient failure is a `503` (a dependency/capacity condition).
const INTERNAL_CODE: &str = "INTERNAL";

/// The one caller code that parks at `404` rather than `400` (an unknown registered-script key).
const NOT_FOUND_CODE: &str = "SCRIPT_NOT_FOUND";

/// Oversized-input request codes → `413 Content Too Large` (regardless of owner).
const OVERSIZE_CODES: [&str; 2] = ["SCRIPT_TOO_LARGE", "CONTEXT_TOO_LARGE"];

/// A classified fault projected onto the HTTP status line, plus whether a `Retry-After`
/// header accompanies it (present on the retryable `5xx` class only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Projected {
    /// The HTTP status code.
    pub(crate) status: u16,
    /// `true` when the response carries a `Retry-After` header (the retry class).
    pub(crate) retry_after: bool,
}

impl Projected {
    /// A retryable `5xx` (carries `Retry-After`).
    const fn retry(status: u16) -> Self {
        Self {
            status,
            retry_after: true,
        }
    }

    /// A non-retryable `4xx` (no `Retry-After`).
    const fn park(status: u16) -> Self {
        Self {
            status,
            retry_after: false,
        }
    }
}

/// Projects a classified fault `(retryable, owner, code)` onto its HTTP status line.
///
/// `retryable = true ⇒ 5xx`: `500` for `INTERNAL` (a box bug), `503` for every other transient
/// failure including capacity/quota — always with `Retry-After`. `retryable = false ⇒ 4xx`: `413`
/// oversize, `409` operator misconfig, `404` unknown script, `400` other caller faults, `422`
/// developer/runtime. Never `429`.
#[must_use]
pub(crate) fn project(retryable: bool, owner: ErrorOwner, code: &str) -> Projected {
    if retryable {
        // The class is 5xx; `owner` never changes it. Only `INTERNAL` splits off to 500.
        let status = if code == INTERNAL_CODE { 500 } else { 503 };
        return Projected::retry(status);
    }
    Projected::park(park_status(owner, code))
}

/// The `4xx` (park) code for a non-retryable fault. Oversize inputs are `413` irrespective of
/// owner; otherwise the owner selects the family and `SCRIPT_NOT_FOUND` is the lone `404`.
/// Operator-owned non-retryable faults are misconfiguration a retry cannot fix, so they park at
/// `409` (D1: an "operator-owned but non-retryable" cell is still park, never `5xx`).
fn park_status(owner: ErrorOwner, code: &str) -> u16 {
    if OVERSIZE_CODES.contains(&code) {
        return 413;
    }
    match owner {
        ErrorOwner::Caller if code == NOT_FOUND_CODE => 404,
        ErrorOwner::Caller => 400,
        ErrorOwner::Operator => 409,
        ErrorOwner::Developer => 422,
    }
}

/// Projects an assembled [`ErrorEnvelope`] — the form every response path already holds, so the
/// projection reads the classification the envelope carries rather than re-deriving it.
#[must_use]
pub(crate) fn project_envelope(envelope: &ErrorEnvelope) -> Projected {
    project(envelope.is_retryable(), envelope.owner(), envelope.code())
}

#[cfg(test)]
mod tests {
    //! The projection as a pure table: every `(retryable, owner, code)` class maps to the expected
    //! status + `Retry-After` presence, and `429` is never produced.

    use super::{Projected, project};
    use runlet_core::errors::ErrorOwner;

    /// Retryable faults are always `5xx` with `Retry-After`: `INTERNAL` → `500`, everything else
    /// (including capacity/quota) → `503`. Never `429`.
    #[test]
    fn retryable_routes_to_5xx_with_retry_after() {
        for (owner, code, expected) in [
            (ErrorOwner::Operator, "INTERNAL", 500),
            (ErrorOwner::Operator, "SHUTTING_DOWN", 503),
            (ErrorOwner::Operator, "EGRESS_UNAVAILABLE", 503),
            (ErrorOwner::Operator, "OVERLOADED", 503),
            (ErrorOwner::Caller, "PARTITION_OVERLOADED", 503),
            (ErrorOwner::Caller, "QUOTA_EXCEEDED", 503),
            (ErrorOwner::Operator, "DB_DEADLOCK", 503),
            (ErrorOwner::Developer, "TIMEOUT", 503),
        ] {
            let projected = project(true, owner, code);
            assert_eq!(
                projected,
                Projected {
                    status: expected,
                    retry_after: true
                },
                "retryable {code} → {expected} + Retry-After"
            );
            assert_ne!(projected.status, 429, "429 is never produced");
        }
    }

    /// Non-retryable faults park at `4xx` with no `Retry-After`, the code chosen by owner + code:
    /// oversize `413`, operator misconfig `409`, unknown script `404`, other caller `400`,
    /// developer/runtime `422`.
    #[test]
    fn non_retryable_routes_to_4xx_park() {
        for (owner, code, expected) in [
            (ErrorOwner::Caller, "SCRIPT_TOO_LARGE", 413),
            (ErrorOwner::Caller, "CONTEXT_TOO_LARGE", 413),
            (ErrorOwner::Operator, "AUTH_REQUEST", 409),
            (ErrorOwner::Operator, "S3_FORBIDDEN", 409),
            (ErrorOwner::Operator, "RESOURCE_KIND_MISMATCH", 409),
            (ErrorOwner::Operator, "EGRESS_PROTOCOL", 409),
            (ErrorOwner::Caller, "SCRIPT_NOT_FOUND", 404),
            (ErrorOwner::Caller, "MALFORMED_REQUEST", 400),
            (ErrorOwner::Caller, "SCRIPT_XOR_KEY", 400),
            (ErrorOwner::Developer, "SYNTAX_ERROR", 422),
            (ErrorOwner::Developer, "MEMORY_LIMIT", 422),
            (ErrorOwner::Developer, "SCRIPT_ERROR", 422),
            (ErrorOwner::Developer, "TIMEOUT", 422),
        ] {
            let projected = project(false, owner, code);
            assert_eq!(
                projected,
                Projected {
                    status: expected,
                    retry_after: false
                },
                "non-retryable {owner:?}/{code} → {expected}, no Retry-After"
            );
            assert!(
                (400..500).contains(&projected.status),
                "non-retryable parks in the 4xx class"
            );
        }
    }
}
