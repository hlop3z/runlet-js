//! The box-side `config.io` interpretation (byo-capabilities: a flat allowlist of logical
//! names): which names are enabled, which need the broker vs box-direct, and the session-open
//! `WireInit` (the flat `resources` list). Name→kind/endpoint resolution itself lives in
//! the broker; box-direct bindings live in the operator's global config.

use super::{RequestIo, wire_init};
use std::collections::HashMap;

/// A `RequestIo` naming a flat list of logical resources.
fn io(names: &[&str]) -> RequestIo {
    RequestIo(names.iter().map(|name| (*name).to_owned()).collect())
}

/// `any()` is true iff a name is listed; `enabled_names()` is the flat allowlist.
#[test]
fn any_and_enabled_names_track_the_flat_allowlist() {
    let listed = io(&["orders", "cache"]);
    assert!(listed.any(), "a named resource means the io port is wired");
    assert_eq!(
        listed.enabled_names(),
        vec!["orders", "cache"],
        "the flat allowlist is the enabled set"
    );

    let empty = RequestIo::default();
    assert!(!empty.any(), "no names → no io port");
    assert!(empty.enabled_names().is_empty(), "no names enabled");
}

/// `broker_names` excludes names bound box-direct in the global local map; a box-direct-only
/// request needs no broker session.
#[test]
fn broker_names_exclude_box_direct_bindings() {
    let mut local = HashMap::new();
    drop(local.insert("pricing".to_owned(), "http://127.0.0.1:8080".to_owned()));
    let listed = io(&["orders", "pricing"]);
    assert_eq!(
        listed.broker_names(&local),
        vec!["orders".to_owned()],
        "only the non-local name goes to the broker"
    );

    let local_only = io(&["pricing"]);
    assert!(
        local_only.broker_names(&local).is_empty(),
        "a box-direct-only request opens no broker session"
    );
}

/// `wire_init` carries the flat resource list, the deadline, and the trusted identity — tenant,
/// actor, and (from a verified contract) principal kind + on-behalf-of.
#[test]
fn wire_init_carries_flat_resources() {
    let init = wire_init(
        vec!["orders".to_owned(), "cache".to_owned()],
        std::time::Duration::from_millis(1500),
        Some("ws_acme"),
        Some("key_9"),
        Some("apikey"),
        Some("u_42"),
    );
    assert_eq!(
        init.resources,
        vec!["orders".to_owned(), "cache".to_owned()]
    );
    assert_eq!(init.timeout_ms, 1500);
    assert_eq!(
        init.tenant.as_deref(),
        Some("ws_acme"),
        "trusted tenant carried on the handshake"
    );
    assert_eq!(
        init.actor.as_deref(),
        Some("key_9"),
        "trusted acting subject (the key id) carried on the handshake"
    );
    assert_eq!(
        init.principal_kind.as_deref(),
        Some("apikey"),
        "verified principal kind carried alongside the actor"
    );
    assert_eq!(
        init.on_behalf_of.as_deref(),
        Some("u_42"),
        "verified acted-for human carried alongside the actor"
    );
}

/// On the single-tenant/loopback path all identity fields are absent from the handshake.
#[test]
fn wire_init_omits_identity_when_absent() {
    let init = wire_init(
        vec!["orders".to_owned()],
        std::time::Duration::from_millis(500),
        None,
        None,
        None,
        None,
    );
    assert!(init.tenant.is_none(), "no tenant on the loopback path");
    assert!(init.actor.is_none(), "no actor on the loopback path");
    assert!(
        init.principal_kind.is_none() && init.on_behalf_of.is_none(),
        "no principal kind / on-behalf-of on the loopback path"
    );
}
