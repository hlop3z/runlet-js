//! `runlet-caps`: the standard capability preset for `runlet-core`.
//!
//! Data only — the six driver-backed capabilities (`db`, `mongo`, `mail`, `redis`, `amq`,
//! `auth`) as composable [`CapabilityDef`]s: each pairs a hand-written JS wrapper with its
//! editor `.d.ts` fragment and a [`Trust`] declaration. **No drivers, no network stack** — the
//! backends live in the `fabricd` sidecar (its `fabric-backends` crate); these defs route every
//! call through the egress mux to whichever backend the host wires (a local `fabric-backends`
//! plugin or the sidecar fallback).
//!
//! The stock `runlet` binary composes [`preset`] onto its `LogicHost`; a custom box picks the
//! subset it wants, or registers its own [`CapabilityDef`]s alongside.

use runlet_core::{CapabilityDef, Trust};

/// The `snake_case` action tokens each capability's JS wrapper sends to `io.call`.
///
/// The single source of truth (D10) the wrapper and the drift fixture share, so a renamed verb
/// is caught at build time rather than becoming a runtime `unknown action` at the `fabricd`
/// backend. The **cross-repo** seam to `fabricd` cannot be compile-checked from this repo; it
/// stays governed by the `runlet-wire` string protocol plus these token lists, which mirror
/// `fabric-backends`' per-backend action names.
pub mod actions {
    /// `db` action tokens.
    pub const DB: &[&str] = &["query", "execute", "begin", "commit", "rollback"];
    /// `mongo` action tokens.
    pub const MONGO: &[&str] = &[
        "find",
        "find_one",
        "count",
        "aggregate",
        "insert_one",
        "insert_many",
        "update_one",
        "update_many",
        "delete_one",
        "delete_many",
    ];
    /// `mail` action tokens.
    pub const MAIL: &[&str] = &["send"];
    /// `redis` action tokens.
    pub const REDIS: &[&str] = &["get", "set", "del", "incr", "expire"];
    /// `amq` action tokens.
    pub const AMQ: &[&str] = &["send", "request"];
    /// `auth` action tokens.
    pub const AUTH: &[&str] = &["user_info", "introspect"];
}

/// The `db` (`Postgres` / `CockroachDB`) capability. Operator-supplied target (no SSRF guard).
#[must_use]
pub fn db() -> CapabilityDef {
    CapabilityDef::new(
        "db",
        include_str!("js/db.js"),
        include_str!("js/db.d.ts"),
        Trust::OperatorSupplied,
    )
}

/// The `mongo` (document database) capability. Operator-supplied target.
#[must_use]
pub fn mongo() -> CapabilityDef {
    CapabilityDef::new(
        "mongo",
        include_str!("js/mongo.js"),
        include_str!("js/mongo.d.ts"),
        Trust::OperatorSupplied,
    )
}

/// The `mail` (SMTP) capability. Operator-supplied relay.
#[must_use]
pub fn mail() -> CapabilityDef {
    CapabilityDef::new(
        "mail",
        include_str!("js/mail.js"),
        include_str!("js/mail.d.ts"),
        Trust::OperatorSupplied,
    )
}

/// The `redis` (key/value) capability. Operator-supplied target.
#[must_use]
pub fn redis() -> CapabilityDef {
    CapabilityDef::new(
        "redis",
        include_str!("js/redis.js"),
        include_str!("js/redis.d.ts"),
        Trust::OperatorSupplied,
    )
}

/// The `amq` (`RabbitMQ` / NATS producer) capability. Operator-supplied broker.
#[must_use]
pub fn amq() -> CapabilityDef {
    CapabilityDef::new(
        "amq",
        include_str!("js/amq.js"),
        include_str!("js/amq.d.ts"),
        Trust::OperatorSupplied,
    )
}

/// The `auth` (OIDC/IAM identity) capability. Operator-supplied issuer.
#[must_use]
pub fn auth() -> CapabilityDef {
    CapabilityDef::new(
        "auth",
        include_str!("js/auth.js"),
        include_str!("js/auth.d.ts"),
        Trust::OperatorSupplied,
    )
}

/// The six standard driver-backed capabilities, in a stable order (the stock server's set).
#[must_use]
pub fn preset() -> Vec<CapabilityDef> {
    vec![db(), mongo(), mail(), redis(), amq(), auth()]
}

#[cfg(test)]
mod tests {
    //! D10/D6 drift guard: every declared action token appears verbatim in its capability's JS
    //! wrapper, so a token renamed on one side without the other fails the build here rather than
    //! at the `fabricd` backend as a runtime `unknown action`.

    use super::{actions, amq, auth, db, mail, mongo, redis};

    /// Asserts each token in `tokens` appears as a quoted string literal in `wrapper`.
    fn assert_tokens_used(name: &str, wrapper: &str, tokens: &[&str]) {
        for token in tokens {
            let quoted = format!("'{token}'");
            assert!(
                wrapper.contains(&quoted),
                "{name} wrapper is missing action token {quoted}"
            );
        }
    }

    /// Every capability's declared action tokens are all present in its wrapper source.
    #[test]
    fn action_tokens_match_wrappers() {
        assert_tokens_used("db", db().js_wrapper(), actions::DB);
        assert_tokens_used("mongo", mongo().js_wrapper(), actions::MONGO);
        assert_tokens_used("mail", mail().js_wrapper(), actions::MAIL);
        assert_tokens_used("redis", redis().js_wrapper(), actions::REDIS);
        assert_tokens_used("amq", amq().js_wrapper(), actions::AMQ);
        assert_tokens_used("auth", auth().js_wrapper(), actions::AUTH);
    }

    /// D11 drift guard: the checked-in `container/types.d.ts` equals what the stock set (the base
    /// fragment + the in-engine `http`/`s3` fragments + the preset's fragments) generates. If this
    /// fails, regenerate the file — see `docs/design/composable-core.md`.
    #[test]
    fn types_dts_is_up_to_date() {
        let defs = super::preset();
        let mut fragments = vec![runlet_core::HTTP_TYPES_DTS, runlet_core::S3_TYPES_DTS];
        fragments.extend(runlet_core::def_fragments(&defs));
        let generated = runlet_core::generate_types_dts(&fragments);
        let checked_in = include_str!("../../../container/types.d.ts");
        assert_eq!(
            generated, checked_in,
            "container/types.d.ts is stale — regenerate from base + fragments"
        );
    }
}
