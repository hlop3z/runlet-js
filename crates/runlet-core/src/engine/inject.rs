//! `$std` bootstrap + value-util and in-engine-capability injection.
//!
//! Populates the sandbox before the user script evals: the `$std` namespace and its lazy value-util
//! accessors (`decimal`/`money`/`datetime`/`text`/`list`/`dict`/`template`/`check`, built on first
//! access), the `json()` bridge + shared FFI primitives, and the enumerated in-engine capabilities
//! (`http`/`s3`). The projection/freeze epilogues and the determinism prune of ambient authorities
//! are wired here (the prune is folded into the lazy builder so an untouched member is never built).

use std::error::Error as StdError;

use rquickjs::{Ctx, Function, Value as JsValue};

#[cfg(feature = "http")]
use crate::http::{self, HttpMetric};
#[cfg(feature = "s3")]
use crate::s3;
#[cfg(feature = "s3")]
use crate::s3::S3Metric;
#[cfg(any(feature = "http", feature = "s3"))]
use crate::sandbox::Collector;
use crate::{check, collections, datetime, decimal, money, sys, template, text};

#[cfg(feature = "_io")]
use super::types::EngineError;
use super::types::{ExecParams, Profile};

/// The `$std` namespace bootstrap (`globalThis.$std = {}` + the `__stdExpose` list) — loaded from
/// `src/js/std.js`. The FIRST injected script: every util/capability IIFE populates `$std`, and the
/// bare globals a script sees are a projection of it (see [`STD_PROJECT`] / [`STD_FREEZE`]).
const STD_BOOTSTRAP: &str = include_str!("../js/std.js");

/// The `$std` → `globalThis` projection epilogue — loaded from `src/js/std_project.js`. Mirrors the
/// curated `__stdExpose` members onto globals (identity-equal references) BEFORE the user script
/// evals, so a handler sees `$`/`json`/`log`/`emit`.
const STD_PROJECT: &str = include_str!("../js/std_project.js");

/// The `$std` deep-freeze + global-lock epilogue — loaded from `src/js/std_freeze.js`. Runs strictly
/// AFTER the determinism prune and BEFORE `handler(ctx)` so the pruned surface is what gets frozen.
const STD_FREEZE: &str = include_str!("../js/std_freeze.js");

/// The lazy-`$std` bootstrap — loaded from `src/js/std_lazy.js`. Installs the captured-intrinsic
/// helpers (`__stdMake`/`__stdFreeze`) and the per-member getter-only accessors on `$std` that build
/// their wrapper lazily via the native `__stdBuild` (see [`inject_lazy_std`]). Evaluated after the
/// eager native FFI bridges are registered and before the projection, so a member is materialized
/// only on first access within a request (D1/D2/D4).
const STD_LAZY: &str = include_str!("../js/std_lazy.js");

/// The `json()` bridge — loaded from `src/js/bridge.js` at compile time.
const JSON_BRIDGE: &str = include_str!("../js/bridge.js");

/// Shared FFI primitives (`__ffi.unwrap`, the `__runlet` tagged-error contract) — loaded from
/// `src/js/ffi.js` at compile time. Injected unconditionally with the bridge so it is present for
/// both egress surfaces (`io.js` and the `s3` bypass), which are gated independently.
const FFI_PRIMITIVES: &str = include_str!("../js/ffi.js");

/// Bootstraps the `$std` namespace object and the `__stdExpose` projection list (D3 step 1). Must
/// run first — every subsequent injector populates `$std`, and the projection/freeze epilogues read
/// `__stdExpose`.
pub(super) fn inject_std_bootstrap(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let boot: JsValue<'_> = qctx.eval(STD_BOOTSTRAP)?;
    drop(boot);
    Ok(())
}

/// Projects the curated `$std` members onto `globalThis` as identity-equal references (D3 step 3),
/// before the user script evals. Only pure, both-profile members are on the list (D2).
pub(super) fn project_std_globals(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let projected: JsValue<'_> = qctx.eval(STD_PROJECT)?;
    drop(projected);
    Ok(())
}

/// Deep-freezes `$std` and locks the projected globals non-writable/non-configurable (D3 step 7),
/// after the determinism prune and before `handler` runs. Idempotent members that were pruned stay
/// pruned; the whole surface becomes tamper-proof for the invocation.
pub(super) fn freeze_std(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let frozen: JsValue<'_> = qctx.eval(STD_FREEZE)?;
    drop(frozen);
    Ok(())
}

/// Injects the always-present JS primitives: the `json(data, error)` bridge and the shared
/// `__ffi` FFI unwrap (both egress wrappers depend on `__ffi`, which is why it lands here rather
/// than in the independently-gated `io`/`s3` injection paths).
pub(super) fn inject_bridge(qctx: &Ctx<'_>) -> Result<(), rquickjs::Error> {
    let bridge: JsValue<'_> = qctx.eval(JSON_BRIDGE)?;
    drop(bridge);
    let ffi: JsValue<'_> = qctx.eval(FFI_PRIMITIVES)?;
    drop(ffi);
    Ok(())
}

/// Registers the eager native FFI bridges and installs the lazy `$std` value-util accessors (D1/D2).
///
/// The bridges (`__decimal`/`__sys`/`__template`) plus the per-request `__default_currency` scalar
/// are cheap and stay eager, so any member's wrapper can resolve its native dependency without
/// forcing another member's build. The wrappers themselves are built lazily: [`register_std_builder`]
/// wires the native `__stdBuild(key)`, and `js/std_lazy.js` installs the getter-only accessors that
/// call it on first access, deep-freeze the result, and memoize it. The deterministic prune of the
/// ambient authorities (`$std.datetime.now`, `$std.crypto.uuid`) is folded into the lazy builder
/// (D4), so no un-pruned alias is ever materialized.
pub(super) fn inject_lazy_std(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    // The eager native halves (the cheap fraction — see the D2 validation in design.md).
    decimal::register_native(qctx)?;
    qctx.globals()
        .set("__default_currency", params.default_currency.unwrap_or(""))?;
    sys::register_native(qctx, params.sys_config)?;
    template::register_native(qctx)?;

    let deterministic = params.profile == Profile::Deterministic;
    let sys_post = match params.sys_config {
        Some(cfg) => sys::context_post_step(cfg)?,
        None => String::new(),
    };
    register_std_builder(qctx, deterministic, &sys_post)?;

    let boot: JsValue<'_> = qctx.eval(STD_LAZY)?;
    drop(boot);
    Ok(())
}

/// Registers the native `__stdBuild(key)` the lazy accessors dispatch on. Each precomputed unit
/// source, when eval'd, parses+executes the unit's wrapper IIFE(s) into a fresh scratch realm
/// (`__stdMake`) and stashes the produced members on `globalThis.__stdBuilt` for the JS getter to
/// read, deep-freeze, and memoize. A build failure surfaces as a thrown exception out of the getter.
fn register_std_builder(
    qctx: &Ctx<'_>,
    deterministic: bool,
    sys_post: &str,
) -> Result<(), rquickjs::Error> {
    let sources = build_unit_sources(deterministic, sys_post);
    let build = Function::new(
        qctx.clone(),
        move |bctx: Ctx<'_>, key: String| -> rquickjs::Result<()> {
            if let Some((_, src)) = sources.iter().find(|(unit_key, _)| *unit_key == key) {
                bctx.eval::<(), _>(src.as_str())?;
            }
            Ok(())
        },
    )?
    .with_name("__stdBuild")?;
    qctx.globals().set("__stdBuild", build)?;
    Ok(())
}

/// Precomputes the shadow-eval source for every lazy build-unit, baking in the per-request sys
/// context post-step and — under the deterministic profile — the ambient-authority prunes. Kept in
/// lockstep with the unit table in `js/std_lazy.js`.
fn build_unit_sources(deterministic: bool, sys_post: &str) -> Vec<(&'static str, String)> {
    // Deterministic prunes, folded into the builder so a pruned member is what gets frozen (D4).
    let det_datetime = if deterministic {
        "if($std.datetime){delete $std.datetime.now;}"
    } else {
        ""
    };
    let det_crypto = if deterministic {
        "if($std.crypto){delete $std.crypto.uuid;}"
    } else {
        ""
    };
    vec![
        shadow_unit("decimal", &["decimal"], decimal::DECIMAL_WRAPPER),
        shadow_unit("money", &["money"], money::MONEY_WRAPPER),
        shadow_unit(
            "sys",
            &["crypto", "env", "secrets"],
            &format!("{}{sys_post}{det_crypto}", sys::SYS_WRAPPER),
        ),
        shadow_unit(
            "datetime",
            &["datetime"],
            &format!("{}{det_datetime}", datetime::DATETIME_WRAPPER),
        ),
        shadow_unit("text", &["text"], text::TEXT_WRAPPER),
        shadow_unit("list", &["list"], collections::LIST_WRAPPER),
        shadow_unit("dict", &["dict"], collections::DICT_WRAPPER),
        shadow_unit("template", &["template"], template::TEMPLATE_WRAPPER),
        shadow_unit("check", &["check"], check::CHECK_WRAPPER),
    ]
}

/// Assembles the shadow-eval source for one build-unit: run `body` (the wrapper IIFE(s) + any
/// post-step) with `$std` lexically rebound to a fresh scratch realm whose prototype is the real
/// `$std` (so dependency reads fire other lazy getters) and whose `members` have own writable slots
/// (so the wrapper's `$std.<name> = X` self-write lands locally), then stash the produced members
/// on `globalThis.__stdBuilt`.
fn shadow_unit(key: &'static str, members: &[&str], body: &str) -> (&'static str, String) {
    let names = members
        .iter()
        .map(|member| format!("{member:?}"))
        .collect::<Vec<_>>()
        .join(",");
    let returned = members
        .iter()
        .map(|member| format!("{member:?}:scratch[{member:?}]"))
        .collect::<Vec<_>>()
        .join(",");
    let src = format!(
        "globalThis.__stdBuilt=(function(scratch){{(function($std){{{body}}})(scratch);\
         return{{{returned}}};}})(__stdMake($std,[{names}]));"
    );
    (key, src)
}

/// Injects the in-engine capabilities `http`/`s3` (subject to the profile).
///
/// These are the enumerated mux-bypass surface (D9): they carry their own in-engine code
/// (`http`'s SSRF-guarded client, `s3`'s `SigV4` signing) rather than routing through the egress
/// mux, and each returns a metric collector captured into the `*_collector` slots. The
/// driver-backed capabilities inject their JS wrappers through the capability registry
/// (`inject_registry`), not here.
#[cfg(feature = "_io")]
pub(super) fn inject_apis(
    qctx: &Ctx<'_>,
    params: &ExecParams<'_>,
    #[cfg(feature = "http")] http_collector: &mut Option<Collector<HttpMetric>>,
    #[cfg(feature = "s3")] s3_collector: &mut Option<Collector<S3Metric>>,
) -> Result<(), EngineError> {
    // Profile enforcement: the deterministic tier gets **no** I/O capability, regardless of
    // what configs an `Invocation` carries — the boundary is enforced here, not trusted to
    // the author (only the pure `$std` helpers, `emit`, and the read-hook remain, injected elsewhere).
    if params.profile != Profile::Full {
        return Ok(());
    }
    #[cfg(feature = "http")]
    if !params.allowed_hosts.is_empty() {
        *http_collector = Some(
            http::inject_http(
                qctx,
                params.allowed_hosts,
                params.max_ops,
                params.allow_private_targets,
                params.wildcard_hosts_allowed,
            )
            .map_err(EngineError::internal)?,
        );
    }
    #[cfg(feature = "s3")]
    if let Some(s3_cfg) = params.s3_config {
        *s3_collector = Some(
            s3::inject_s3(qctx, s3_cfg, params.max_ops, params.allow_private_targets)
                .map_err(EngineError::internal)?,
        );
    }
    Ok(())
}
