//! End-to-end tests of the lazy-`$std` path (change `lazy-std-injection`) driven through the
//! public [`crate::host::LogicHost::run`], exercising the full per-request injection: lazy
//! getter-only accessors, the `$` funnel + identity, per-member deep-freeze at materialization,
//! the locked container, reachability of every member on first access (incl. via destructuring),
//! and the determinism-aware builder (the pruned variant is what materializes).

use std::sync::Arc;

use serde_json::Value;

use super::{ExecOutcome, Profile};
use crate::config::EngineConfig;
use crate::host::{HostSettings, Invocation, LogicHost};
use crate::modules::ModuleRegistry;
use crate::pool::JsPool;
use crate::registry::ScriptRegistry;

/// Builds a minimal capability-free host (value-utils need no egress).
fn host() -> LogicHost {
    let mut config = EngineConfig::default();
    config
        .resolve_limits()
        .unwrap_or_else(|_err| unreachable!("default engine limits must resolve"));
    let modules = Arc::new(ModuleRegistry::default());
    let pool = JsPool::new(config, modules).unwrap_or_else(|_err| unreachable!("pool must build"));
    let settings = HostSettings {
        limits: config,
        allow_private_targets: false,
    };
    LogicHost::new(pool, Arc::new(ScriptRegistry::default()), settings)
}

/// Runs `script` under `profile` (default currency USD) and returns the success envelope's
/// `data`; panics with the classified error on the failure path so a broken build is loud.
fn run_data(host: &LogicHost, script: &str, profile: Profile) -> Value {
    let inv = Invocation::inline(script, "{}")
        .profile(profile)
        .default_currency("USD");
    let outcome = host
        .run(inv)
        .unwrap_or_else(|_err| unreachable!("invocation must run"));
    match outcome.result {
        ExecOutcome::Success(json) => {
            let env: Value = serde_json::from_str(&json)
                .unwrap_or_else(|_err| unreachable!("success envelope must be JSON"));
            env.get("data").cloned().unwrap_or(Value::Null)
        }
        ExecOutcome::Error(err) => panic!("handler errored: {err:?}"),
    }
}

/// A touched member (`$`/money) builds and behaves identically to eager injection; identity
/// (`$ === $std.money`) holds; the built member is deep-frozen before the handler can mutate it;
/// and the container is locked (no add/delete/replace of a member).
#[test]
fn identity_freeze_and_container_lock() {
    const SCRIPT: &str = "function handler(ctx){ \
            var same = ($ === $std.money); \
            var minor = $(10, 'USD').to_minor(); \
            var before = $std.money; \
            try { $std.money = 42; } catch(e){} \
            var replaceNoop = ($std.money === before); \
            try { delete $std.decimal; } catch(e){} \
            var deleteNoop = (typeof $std.decimal === 'function'); \
            try { $std.newthing = 1; } catch(e){} \
            var addNoop = (typeof $std.newthing === 'undefined'); \
            try { $std.money.__x = 1; } catch(e){} \
            var frozen = Object.isFrozen($std.money) && (typeof $std.money.__x === 'undefined'); \
            return json({ same: same, minor: String(minor), frozen: frozen, \
              replaceNoop: replaceNoop, deleteNoop: deleteNoop, addNoop: addNoop }); \
        }";
    let data = run_data(&host(), SCRIPT, Profile::Full);
    assert_eq!(data["same"], Value::Bool(true), "$ === $std.money");
    assert_eq!(data["frozen"], Value::Bool(true), "member deep-frozen");
    assert_eq!(
        data["replaceNoop"],
        Value::Bool(true),
        "member not replaceable"
    );
    assert_eq!(
        data["deleteNoop"],
        Value::Bool(true),
        "member not deletable"
    );
    assert_eq!(
        data["addNoop"],
        Value::Bool(true),
        "container not extensible"
    );
    assert_eq!(
        data["minor"],
        Value::String("1000".to_owned()),
        "money math intact"
    );
}

/// Every value-util member is reachable on first access (lazy build on demand), including via
/// destructuring (a read that forces the build), and behaves as before.
#[test]
fn all_members_reachable_and_destructuring_builds() {
    const SCRIPT: &str = "function handler(ctx){ \
            var kinds = { \
              decimal: typeof $std.decimal, money: typeof $std.money, \
              datetime: typeof $std.datetime, text: typeof $std.text, \
              list: typeof $std.list, dict: typeof $std.dict, \
              template: typeof $std.template, check: typeof $std.check, \
              crypto: typeof $std.crypto, env: typeof $std.env, secrets: typeof $std.secrets }; \
            var d = $std.decimal(3).add($std.decimal(4)).toString(); \
            var t = $std.text('Hi').lower().value; \
            var tmpl = typeof $std.template.text; \
            var forced = (function(){ var { money, list } = $std; \
              return typeof money === 'function' && typeof list === 'function'; })(); \
            return json({ kinds: kinds, d: d, t: t, tmpl: tmpl, forced: forced }); \
        }";
    let data = run_data(&host(), SCRIPT, Profile::Full);
    let kinds = &data["kinds"];
    for member in [
        "decimal", "money", "datetime", "text", "list", "dict", "check",
    ] {
        assert_eq!(
            kinds[member],
            Value::String("function".to_owned()),
            "{member} callable"
        );
    }
    for member in ["template", "crypto", "env", "secrets"] {
        assert_eq!(
            kinds[member],
            Value::String("object".to_owned()),
            "{member} object"
        );
    }
    assert_eq!(
        data["d"],
        Value::String("7".to_owned()),
        "decimal math intact"
    );
    assert_eq!(
        data["t"],
        Value::String("hi".to_owned()),
        "text util intact"
    );
    assert_eq!(
        data["tmpl"],
        Value::String("function".to_owned()),
        "template namespace intact"
    );
    assert_eq!(
        data["forced"],
        Value::Bool(true),
        "destructuring forces the build"
    );
}

/// Under the deterministic profile the lazy build materializes the already-pruned variant:
/// `$std.datetime.now` and `$std.crypto.uuid` are absent, and the JS-builtin sanitizer still
/// neutralizes `Math.random` and no-arg `new Date()`.
#[test]
fn deterministic_build_omits_ambient_authorities() {
    const SCRIPT: &str = "function handler(ctx){ \
            return json({ \
              now: typeof $std.datetime.now, \
              uuid: typeof $std.crypto.uuid, \
              rand: typeof Math.random, \
              date: (function(){ try { new Date(); return 'ok'; } catch(e){ return 'blocked'; } })() \
            }); \
        }";
    let data = run_data(&host(), SCRIPT, Profile::Deterministic);
    assert_eq!(
        data["now"],
        Value::String("undefined".to_owned()),
        "no datetime.now"
    );
    assert_eq!(
        data["uuid"],
        Value::String("undefined".to_owned()),
        "no crypto.uuid"
    );
    assert_eq!(
        data["rand"],
        Value::String("undefined".to_owned()),
        "no Math.random"
    );
    assert_eq!(
        data["date"],
        Value::String("blocked".to_owned()),
        "no wall-clock new Date()"
    );
}

/// Under the full profile the same members keep their ambient authorities — proving the prune is
/// gated on the profile, not unconditional.
#[test]
fn full_build_keeps_ambient_authorities() {
    const SCRIPT: &str = "function handler(ctx){ \
            return json({ now: typeof $std.datetime.now, uuid: typeof $std.crypto.uuid }); \
        }";
    let data = run_data(&host(), SCRIPT, Profile::Full);
    assert_eq!(
        data["now"],
        Value::String("function".to_owned()),
        "datetime.now present"
    );
    assert_eq!(
        data["uuid"],
        Value::String("function".to_owned()),
        "crypto.uuid present"
    );
}

/// A handler that reassigns an exposed global (`log = 5`) does not change the injected binding.
#[test]
fn exposed_globals_cannot_be_reassigned() {
    const SCRIPT: &str = "function handler(ctx){ \
            try { log = 5; } catch(e){} \
            try { json = 5; } catch(e){} \
            return json({ log: typeof log, json: typeof json }); \
        }";
    let data = run_data(&host(), SCRIPT, Profile::Full);
    assert_eq!(
        data["log"],
        Value::String("object".to_owned()),
        "log stays the leveled-logger object (reassignment rejected)"
    );
    assert_eq!(
        data["json"],
        Value::String("function".to_owned()),
        "json stays the bridge (reassignment rejected)"
    );
}
