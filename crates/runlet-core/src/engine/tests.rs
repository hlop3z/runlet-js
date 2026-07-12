//! Verifies the wall-clock interrupt preempts a catastrophic-backtracking regex.
//!
//! `QuickJS`'s libregexp does not yield to the interrupt handler on its own, so this proves
//! that a `ReDoS` pattern is still bounded by the execution timeout rather than pinning a
//! `spawn_blocking` thread until the match completes.

use rquickjs::{Context, Runtime, Value as JsValue};
use std::time::{Duration, Instant};

/// A `(a+)+$` pattern over a non-matching tail backtracks exponentially; with 30 leading
/// `a`s it would run for several seconds uninterrupted, so prompt completion proves the
/// interrupt aborted the match rather than letting it run to the end.
#[test]
fn catastrophic_regex_is_interrupted() {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let timeout = Duration::from_millis(250);
    let start = Instant::now();
    runtime.set_interrupt_handler(Some(Box::new(move || start.elapsed() > timeout)));
    let ctx = Context::full(&runtime).unwrap_or_else(|_err| unreachable!());
    let script = format!("/(a+)+$/.test(\"{}!\")", "a".repeat(30));
    ctx.with(|qctx| {
        let res: Result<JsValue<'_>, _> = qctx.eval(script.as_bytes());
        assert!(
            res.is_err(),
            "the wall-clock interrupt must abort a catastrophic regex"
        );
    });
    runtime.set_interrupt_handler(None);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "regex was not preempted promptly (interrupt did not fire during matching)"
    );
}

/// Mechanism-level regression for the lazy-`$std` accessor (change `lazy-std-injection`), using
/// fake wrappers so it can assert what the end-to-end `LogicHost` tests cannot observe: (A)
/// `qctx.eval` works after the JS `eval` global is removed and (B) while *nested* inside a
/// running handler, so a native getter can lazily parse+build a member; (C) a JS accessor
/// calling a native `__build` memoizes+freezes; (D) `Object.create($std)` proto-delegation fires
/// inherited lazy getters (build-time dep resolve) while a pre-defined own writable slot captures
/// the wrapper's `$std.<name> = X` self-write; (E) the `$` global funnels to `$std.money`
/// (identity); (F) a member is built **at most once** and an **untouched member is never built**
/// (the build counters — unobservable through the public host API). Mirrors the real
/// `js/std_lazy.js` accessor logic.
#[test]
fn lazy_std_accessor_mechanism() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rquickjs::{Ctx, Function};

    // Fake wrapper sources — `decimal` has no dep; `money` reads `$std.decimal` at *build* time
    // (top of the IIFE) exactly like the real `money.js`; `template` is the untouched member.
    const DECIMAL_WRAPPER: &str =
        "(function(){ $std.decimal = { tag: 'dec', add: function(a,b){ return a+b; } }; })()";
    const MONEY_WRAPPER: &str = "(function(){ var D = $std.decimal; \
             $std.money = { tag: 'money', dep: D.tag, plus: function(a,b){ return D.add(a,b); } }; })()";
    const TEMPLATE_WRAPPER: &str = "(function(){ $std.template = { tag: 'tmpl' }; })()";

    fn wrapper_for(name: &str) -> &'static str {
        match name {
            "decimal" => DECIMAL_WRAPPER,
            "money" => MONEY_WRAPPER,
            _template => TEMPLATE_WRAPPER,
        }
    }

    // Shadow-eval source: captures the wrapper's `$std.<name> = X` self-write on a fresh own
    // writable slot while dependency reads delegate to the real `$std` (firing other getters).
    fn shadow_source(name: &str, wrapper: &str) -> String {
        format!(
            "globalThis.__built = (function(real){{ \
                   var scratch = Object.create(real); \
                   Object.defineProperty(scratch, '{name}', \
                     {{ value: undefined, writable: true, configurable: true, enumerable: true }}); \
                   (function($std){{ {wrapper} }})(scratch); \
                   return scratch['{name}']; \
                 }})($std);"
        )
    }

    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let ctx = Context::full(&runtime).unwrap_or_else(|_err| unreachable!());

    let counts: Arc<[(&str, AtomicUsize); 3]> = Arc::new([
        ("decimal", AtomicUsize::new(0)),
        ("money", AtomicUsize::new(0)),
        ("template", AtomicUsize::new(0)),
    ]);

    let result = ctx.with(|qctx| {
        let boot: JsValue<'_> = qctx
            .eval("globalThis.$std = {};")
            .unwrap_or_else(|_err| unreachable!());
        drop(boot);

        let counts_ref = Arc::clone(&counts);
        let build = Function::new(qctx.clone(), move |bctx: Ctx<'_>, name: String| {
            for (n, c) in counts_ref.iter() {
                if *n == name {
                    let _prev = c.fetch_add(1, Ordering::Relaxed);
                }
            }
            let src = shadow_source(&name, wrapper_for(&name));
            bctx.eval::<(), _>(src)
                .unwrap_or_else(|_err| unreachable!());
        })
        .unwrap_or_else(|_err| unreachable!())
        .with_name("__build")
        .unwrap_or_else(|_err| unreachable!());
        qctx.globals()
            .set("__build", build)
            .unwrap_or_else(|_err| unreachable!());

        let bootstrap = "\
                ['decimal','money','template'].forEach(function(name){ \
                  var cache, built = false; \
                  Object.defineProperty($std, name, { \
                    get: function(){ if(!built){ __build(name); \
                      cache = Object.freeze(globalThis.__built); globalThis.__built = undefined; \
                      built = true; } return cache; }, \
                    enumerable: true, configurable: false \
                  }); \
                }); \
                Object.defineProperty(globalThis, '$', { \
                  get: function(){ return $std.money; }, enumerable: true, configurable: true \
                });";
        let installed: JsValue<'_> = qctx.eval(bootstrap).unwrap_or_else(|_err| unreachable!());
        drop(installed);

        // Harden: remove the JS eval/Proxy globals, then lock the container.
        let globals = qctx.globals();
        globals.remove("eval").unwrap_or_else(|_err| unreachable!());
        let _proxy = globals.remove("Proxy");
        let frozen: JsValue<'_> = qctx
            .eval("Object.freeze($std);")
            .unwrap_or_else(|_err| unreachable!());
        drop(frozen);

        // The "handler": touch `$` (→ money → decimal), never touch `template`.
        let handler = "(function(){ \
                var out = {}; \
                out.plus = $.plus(2, 3); \
                out.identity = ($ === $std.money); \
                out.dep = $std.money.dep; \
                try { $std.money.tag = 'x'; } catch(e) {} \
                out.frozen = ($std.money.tag === 'money'); \
                try { $std.newthing = 1; } catch(e) {} \
                out.locked = (typeof $std.newthing === 'undefined'); \
                return JSON.stringify(out); \
            })()";
        qctx.eval::<String, _>(handler)
            .unwrap_or_else(|_err| unreachable!())
    });

    runtime.set_interrupt_handler(None);

    assert_eq!(
        result, r#"{"plus":5,"identity":true,"dep":"dec","frozen":true,"locked":true}"#,
        "lazy build must be transparent: identity holds, build-dep resolved, member frozen, \
             container locked"
    );
    assert_eq!(
        counts[0].1.load(Ordering::Relaxed),
        1,
        "decimal built exactly once (via money's build-time dep)"
    );
    assert_eq!(
        counts[1].1.load(Ordering::Relaxed),
        1,
        "money built exactly once"
    );
    assert_eq!(
        counts[2].1.load(Ordering::Relaxed),
        0,
        "untouched template member must never be built"
    );
}
