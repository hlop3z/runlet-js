//! `$std` bootstrap + value-util and in-engine-capability injection.
//!
//! Populates the sandbox before the user script evals: the `$std` namespace and its lazy value-util
//! accessors (`decimal`/`money`/`datetime`/`text`/`list`/`dict`/`template`/`check`, built on first
//! access), the `json()` bridge + shared FFI primitives, and the enumerated in-engine capabilities
//! (`http`/`s3`). The projection/freeze epilogues and the determinism prune of ambient authorities
//! are wired here (the prune is folded into the lazy builder so an untouched member is never built).

use std::error::Error as StdError;
use std::sync::Arc;

use rquickjs::{Context, Ctx, Function, Module, Runtime, Value as JsValue, WriteOptions};

use super::classify::eval_surface_bytecode;

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

/// Precompiled `QuickJS` module bytecode for the injected framework surface — the always-parsed
/// scaffolding the engine re-injects into every fresh context. Compiled once at pool warm-up
/// ([`compile_surface`]) and loaded per request via [`eval_surface_bytecode`], swapping a
/// per-request parse+compile of the framework JS for a bytecode load (~3× cheaper on multi-KB
/// sources).
///
/// The reuse is of compiled **code only, never state**: every request still receives a fresh
/// context, so no global or prototype mutation survives between requests (the modified
/// per-request-isolation requirement in `specs/execution`).
#[derive(Debug)]
pub(crate) struct PrecompiledSurface {
    /// `std.js` — the `$std` bootstrap.
    std: Arc<[u8]>,
    /// `bridge.js` — the `json()` bridge.
    bridge: Arc<[u8]>,
    /// `ffi.js` — the shared `__ffi` FFI primitives.
    ffi: Arc<[u8]>,
    /// `std_lazy.js` — the lazy `$std` accessor installer.
    std_lazy: Arc<[u8]>,
    /// `std_project.js` — the `$std`→`globalThis` projection epilogue.
    std_project: Arc<[u8]>,
    /// `std_freeze.js` — the deep-freeze/lock epilogue.
    std_freeze: Arc<[u8]>,
    /// Precompiled value-util build-units (group 4): the profile/request-independent wrappers
    /// (`shared_build_units`) as `(key, bytecode)`, loaded on a member's first access instead of
    /// re-parsing its wrapper source. `sys` is absent (its body carries per-request env/secrets);
    /// `datetime` is carried as the two profile variants below.
    build_units: Vec<(&'static str, Arc<[u8]>)>,
    /// `datetime` build-unit under `Profile::Full` (no prune).
    datetime_full: Arc<[u8]>,
    /// `datetime` build-unit under `Profile::Deterministic` (`$std.datetime.now` pruned).
    datetime_deterministic: Arc<[u8]>,
}

impl PrecompiledSurface {
    /// The precompiled value-util build-unit blobs for `deterministic`, as `(key, bytecode)` — the
    /// shared units plus the profile-correct `datetime` variant. `sys` is intentionally absent (it
    /// stays source-parsed). Cheap: clones `Arc` pointers, not bytes.
    fn build_unit_blobs(&self, deterministic: bool) -> Vec<(&'static str, Arc<[u8]>)> {
        let mut units = self.build_units.clone();
        let datetime = if deterministic {
            Arc::clone(&self.datetime_deterministic)
        } else {
            Arc::clone(&self.datetime_full)
        };
        units.push(("datetime", datetime));
        units
    }
}

/// Compiles the framework surface to module bytecode once, on a throwaway context over `runtime`.
/// Called at [`crate::pool::JsPool::new`] warm-up. A compile failure fails pool construction — the
/// box must not boot if it cannot compile its own fixed surface (fail closed).
///
/// # Errors
///
/// Returns an error if context creation or any unit's parse/compile/serialize fails.
pub(crate) fn compile_surface(
    runtime: &Runtime,
) -> Result<PrecompiledSurface, Box<dyn StdError + Send + Sync>> {
    let ctx = Context::full(runtime)?;
    let surface = ctx.with(|qctx| -> Result<PrecompiledSurface, rquickjs::Error> {
        let build_units = shared_build_units()
            .iter()
            .map(|(key, members, wrapper)| -> Result<_, rquickjs::Error> {
                Ok((
                    *key,
                    compile_unit(&qctx, key, &shadow_src(members, wrapper))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PrecompiledSurface {
            std: compile_unit(&qctx, "std", STD_BOOTSTRAP)?,
            bridge: compile_unit(&qctx, "bridge", JSON_BRIDGE)?,
            ffi: compile_unit(&qctx, "ffi", FFI_PRIMITIVES)?,
            std_lazy: compile_unit(&qctx, "std_lazy", STD_LAZY)?,
            std_project: compile_unit(&qctx, "std_project", STD_PROJECT)?,
            std_freeze: compile_unit(&qctx, "std_freeze", STD_FREEZE)?,
            build_units,
            datetime_full: compile_unit(
                &qctx,
                "datetime",
                &shadow_src(&["datetime"], datetime::DATETIME_WRAPPER),
            )?,
            datetime_deterministic: compile_unit(
                &qctx,
                "datetime",
                &shadow_src(
                    &["datetime"],
                    &format!("{}{DET_DATETIME_PRUNE}", datetime::DATETIME_WRAPPER),
                ),
            )?,
        })
    })?;
    Ok(surface)
}

/// Parses+compiles one framework unit to serialized bytecode. The module name is cosmetic — these
/// modules are loaded directly for their global side effects, never `import`ed by name.
fn compile_unit(qctx: &Ctx<'_>, name: &str, source: &str) -> Result<Arc<[u8]>, rquickjs::Error> {
    let declared = Module::declare(qctx.clone(), name, source)?;
    let bytes = declared.write(WriteOptions::default())?;
    Ok(Arc::from(bytes.into_boxed_slice()))
}

/// Injects one framework unit: loads its precompiled surface bytecode when the pool provided a
/// surface, else parses `source` verbatim. The `None` fallback keeps consumers that build no
/// surface (and the equivalence golden test) working, and makes the two paths trivially
/// comparable. Behavior is identical either way — the module runs the same side effects into the
/// same fresh context.
fn inject_unit(
    qctx: &Ctx<'_>,
    surface: Option<&[u8]>,
    source: &str,
) -> Result<(), rquickjs::Error> {
    if let Some(bytecode) = surface {
        return eval_surface_bytecode(qctx, bytecode);
    }
    let value: JsValue<'_> = qctx.eval(source)?;
    drop(value);
    Ok(())
}

/// Bootstraps the `$std` namespace object and the `__stdExpose` projection list (D3 step 1). Must
/// run first — every subsequent injector populates `$std`, and the projection/freeze epilogues read
/// `__stdExpose`.
pub(super) fn inject_std_bootstrap(
    qctx: &Ctx<'_>,
    surface: Option<&PrecompiledSurface>,
) -> Result<(), rquickjs::Error> {
    inject_unit(qctx, surface.map(|sf| sf.std.as_ref()), STD_BOOTSTRAP)
}

/// Projects the curated `$std` members onto `globalThis` as identity-equal references (D3 step 3),
/// before the user script evals. Only pure, both-profile members are on the list (D2).
pub(super) fn project_std_globals(
    qctx: &Ctx<'_>,
    surface: Option<&PrecompiledSurface>,
) -> Result<(), rquickjs::Error> {
    inject_unit(qctx, surface.map(|sf| sf.std_project.as_ref()), STD_PROJECT)
}

/// Deep-freezes `$std` and locks the projected globals non-writable/non-configurable (D3 step 7),
/// after the determinism prune and before `handler` runs. Idempotent members that were pruned stay
/// pruned; the whole surface becomes tamper-proof for the invocation.
pub(super) fn freeze_std(
    qctx: &Ctx<'_>,
    surface: Option<&PrecompiledSurface>,
) -> Result<(), rquickjs::Error> {
    inject_unit(qctx, surface.map(|sf| sf.std_freeze.as_ref()), STD_FREEZE)
}

/// Injects the always-present JS primitives: the `json(data, error)` bridge and the shared
/// `__ffi` FFI unwrap (both egress wrappers depend on `__ffi`, which is why it lands here rather
/// than in the independently-gated `io`/`s3` injection paths).
pub(super) fn inject_bridge(
    qctx: &Ctx<'_>,
    surface: Option<&PrecompiledSurface>,
) -> Result<(), rquickjs::Error> {
    inject_unit(qctx, surface.map(|sf| sf.bridge.as_ref()), JSON_BRIDGE)?;
    inject_unit(qctx, surface.map(|sf| sf.ffi.as_ref()), FFI_PRIMITIVES)?;
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
    register_std_builder(qctx, deterministic, &sys_post, params.surface)?;

    inject_unit(
        qctx,
        params.surface.map(|sf| sf.std_lazy.as_ref()),
        STD_LAZY,
    )?;
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
    surface: Option<&PrecompiledSurface>,
) -> Result<(), rquickjs::Error> {
    let sources = build_unit_sources(deterministic, sys_post);
    // Precompiled build-unit bytecode for this profile (empty when no surface ⇒ all source-parse).
    // A key present here loads bytecode; the rest (always `sys`, and every unit under the fallback)
    // parse `sources`. Both produce the identical `globalThis.__stdBuilt` the JS getter reads.
    let blobs = surface
        .map(|sf| sf.build_unit_blobs(deterministic))
        .unwrap_or_default();
    let build = Function::new(
        qctx.clone(),
        move |bctx: Ctx<'_>, key: String| -> rquickjs::Result<()> {
            if let Some((_, blob)) = blobs.iter().find(|(unit_key, _)| *unit_key == key) {
                return eval_surface_bytecode(&bctx, blob);
            }
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

/// Deterministic prune of `$std.datetime.now`, appended to the datetime unit body under
/// `Profile::Deterministic` so the pruned member is what gets frozen (D4).
const DET_DATETIME_PRUNE: &str = "if($std.datetime){delete $std.datetime.now;}";
/// Deterministic prune of `$std.crypto.uuid`, appended to the sys unit body under
/// `Profile::Deterministic`.
const DET_CRYPTO_PRUNE: &str = "if($std.crypto){delete $std.crypto.uuid;}";

/// The profile- AND request-independent value-util build-units `(key, members, wrapper)`: their
/// shadow-eval source is a pure function of the wrapper const, so it precompiles to bytecode once
/// (group 4). `sys` (per-request env/secrets) and `datetime` (a per-profile prune) are handled
/// separately. Kept in lockstep with the unit table in `js/std_lazy.js`.
const fn shared_build_units() -> [(&'static str, &'static [&'static str], &'static str); 7] {
    [
        ("decimal", &["decimal"], decimal::DECIMAL_WRAPPER),
        ("money", &["money"], money::MONEY_WRAPPER),
        ("text", &["text"], text::TEXT_WRAPPER),
        ("list", &["list"], collections::LIST_WRAPPER),
        ("dict", &["dict"], collections::DICT_WRAPPER),
        ("template", &["template"], template::TEMPLATE_WRAPPER),
        ("check", &["check"], check::CHECK_WRAPPER),
    ]
}

/// Precomputes the shadow-eval source for every lazy build-unit (the source-parse fallback path
/// and the always-source `sys` unit), baking in the per-request sys context post-step and — under
/// the deterministic profile — the ambient-authority prunes.
fn build_unit_sources(deterministic: bool, sys_post: &str) -> Vec<(&'static str, String)> {
    let det_datetime = if deterministic {
        DET_DATETIME_PRUNE
    } else {
        ""
    };
    let det_crypto = if deterministic { DET_CRYPTO_PRUNE } else { "" };
    let mut units: Vec<(&'static str, String)> = shared_build_units()
        .iter()
        .map(|(key, members, wrapper)| (*key, shadow_src(members, wrapper)))
        .collect();
    units.push((
        "sys",
        shadow_src(
            &["crypto", "env", "secrets"],
            &format!("{}{sys_post}{det_crypto}", sys::SYS_WRAPPER),
        ),
    ));
    units.push((
        "datetime",
        shadow_src(
            &["datetime"],
            &format!("{}{det_datetime}", datetime::DATETIME_WRAPPER),
        ),
    ));
    units
}

/// Assembles the shadow-eval source for one build-unit: run `body` (the wrapper IIFE(s) + any
/// post-step) with `$std` lexically rebound to a fresh scratch realm whose prototype is the real
/// `$std` (so dependency reads fire other lazy getters) and whose `members` have own writable slots
/// (so the wrapper's `$std.<name> = X` self-write lands locally), then stash the produced members
/// on `globalThis.__stdBuilt`.
fn shadow_src(members: &[&str], body: &str) -> String {
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
    format!(
        "globalThis.__stdBuilt=(function(scratch){{(function($std){{{body}}})(scratch);\
         return{{{returned}}};}})(__stdMake($std,[{names}]));"
    )
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
