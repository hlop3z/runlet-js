//! §3 response mirror: `logs` is present iff the trusted gateway requested capture, on both the
//! 2xx and non-2xx paths, and absent otherwise (byte-compatible with the prior contract). Also
//! the caller-can't-force-capture policy (an untrusted request never captures).

use super::{Effect, ErrorEnvelope, LogEntry, LogPolicy, Meta, Response, SystemErrorResponse};
use crate::identity::{RunMode, TrustedIdentity};
use runlet_core::engine::LogLevel;
use runlet_core::errors::{ErrorCategory, ErrorOwner, ErrorSource};
use serde_json::value::RawValue;

/// One representative captured entry.
fn entry() -> LogEntry {
    LogEntry {
        level: LogLevel::Info,
        template: "hi {n}".to_owned(),
        properties: RawValue::from_string("{\"n\":1}".to_owned())
            .unwrap_or_else(|_e| unreachable!()),
        message: "hi 1".to_owned(),
        seq: 0,
        offset_us: None,
    }
}

/// A minimal meta for serialization.
fn meta() -> Meta {
    Meta::new("trace-1".to_owned(), 0, 0, 0)
}

/// A raw `null` value for the success envelope fields.
fn raw_null() -> Box<RawValue> {
    RawValue::from_string("null".to_owned()).unwrap_or_else(|_e| unreachable!())
}

/// A success response with a captured mirror serializes a top-level `logs` array; without it the
/// field is entirely absent (byte-compatible with `{data, error, meta}`).
#[test]
fn success_mirror_present_only_when_captured() {
    let logs = [entry()];
    let data = raw_null();
    let error = raw_null();
    let no_effects: [Effect; 0] = [];
    let with = Response {
        data: &data,
        error: &error,
        meta: meta(),
        effects: &no_effects,
        logs: Some(&logs),
    };
    let json = serde_json::to_string(&with).unwrap_or_default();
    assert!(
        json.contains("\"logs\""),
        "captured run carries logs: {json}"
    );
    assert!(json.contains("hi 1"), "the entry is serialized");

    let without = Response {
        data: &data,
        error: &error,
        meta: meta(),
        effects: &no_effects,
        logs: None,
    };
    let json = serde_json::to_string(&without).unwrap_or_default();
    assert!(
        !json.contains("\"logs\""),
        "no capture ⇒ no logs field: {json}"
    );
}

/// A non-2xx (system error) response carries the partial trail when captured (capture-on-failure)
/// and omits the field otherwise.
#[test]
fn error_mirror_present_only_when_captured() {
    let logs = [entry()];
    let no_effects: [Effect; 0] = [];
    let script_error = || {
        ErrorEnvelope::new(
            ErrorCategory::Script,
            ErrorSource::Handler,
            "SCRIPT_ERROR".to_owned(),
            false,
            ErrorOwner::Developer,
        )
    };
    let with = SystemErrorResponse {
        data: None,
        error: script_error(),
        meta: meta(),
        effects: &no_effects,
        logs: Some(&logs),
    };
    let json = serde_json::to_string(&with).unwrap_or_default();
    assert!(
        json.contains("\"logs\""),
        "captured error carries the trail: {json}"
    );

    let without = SystemErrorResponse {
        data: None,
        error: script_error(),
        meta: meta(),
        effects: &no_effects,
        logs: None,
    };
    let json = serde_json::to_string(&without).unwrap_or_default();
    assert!(
        !json.contains("\"logs\""),
        "no capture ⇒ no logs field: {json}"
    );
}

/// A request with no trusted identity (the caller-asserted / single-tenant path) never captures
/// and is always live — a caller cannot force the mirror or pick the mode.
#[test]
fn untrusted_request_never_captures() {
    let policy = LogPolicy::resolve(None);
    assert!(!policy.capture, "no gateway ⇒ no capture");
    assert_eq!(policy.mode, RunMode::Live, "no gateway ⇒ live");
}

/// The trusted capture flag drives the mirror; the mode is carried through for stream routing.
#[test]
fn trusted_capture_flag_drives_policy() {
    let id = TrustedIdentity {
        capture: true,
        mode: RunMode::Test,
        ..TrustedIdentity::default()
    };
    let policy = LogPolicy::resolve(Some(&id));
    assert!(policy.capture, "trusted capture requests the mirror");
    assert_eq!(
        policy.mode,
        RunMode::Test,
        "test mode is response-mirror-only"
    );
}
