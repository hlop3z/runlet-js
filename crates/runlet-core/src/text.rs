//! The first-class `text` value-util for the `QuickJS` sandbox.
//!
//! `text` is the always-on string value-util at `$std.text` (beside `$`/`$std.decimal`/`$std.datetime`): a callable
//! factory (`text(input)`) that wraps a value as an immutable string with chainable, `snake_case`
//! methods. The method names are Python-flavored renames of native JS string operations; the
//! semantics are JavaScript's (UTF-16 code units, Unicode-default casing). A small set of
//! ERP-common shaping verbs (`slugify`/`mask`/`collapse`/`truncate`/padding) composes from the
//! same primitives.
//!
//! Pure JS (`js/text.js`) — no `__sys` bridge and no Rust math: unlike `datetime`, every operation
//! is expressible over `String.prototype`, so this injector only evals the wrapper. It is injected
//! under **both** profiles: `text` touches no clock, no randomness, and no ambient authority, so
//! the deterministic sanitizer (`js/determinism.js`) removes nothing from it.

use std::error::Error;

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/text.js` at compile time. Depends on no other injected global.
const TEXT_WRAPPER: &str = include_str!("js/text.js");

/// Injects `$std.text`. Order-independent among the pure value-utils (no bridge dependency).
///
/// # Errors
///
/// Returns an error if the JS eval fails.
pub fn inject_text(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let wrapper: JsValue<'_> = qctx.eval(TEXT_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Behavioral tests for the `text` value-util, driven end-to-end through the QuickJS engine
    //! (the surface is pure JS, so there is no Rust `dispatch` to unit-test directly). Each test
    //! injects the wrapper and evals an assertion expression.

    use rquickjs::{Context, Runtime};

    use super::inject_text;

    /// The deterministic-profile sanitizer, evaled to prove `text` survives it untouched.
    const DETERMINISM: &str = include_str!("js/determinism.js");

    /// Bootstraps `$std` (the wrapper now populates it, not `globalThis`), injects `text`, then
    /// mirrors it back to a bare `text` global so these behavioral expressions read naturally. In
    /// production the engine does the bootstrap + projection; here the harness stands in for it.
    fn inject(qctx: &rquickjs::Ctx<'_>) {
        qctx.eval::<(), _>("globalThis.$std = {};")
            .expect("bootstrap std");
        inject_text(qctx).expect("inject text");
        qctx.eval::<(), _>("globalThis.text = $std.text;")
            .expect("project text");
    }

    /// Inject `text` and eval a JS expression that yields a string.
    fn run(expr: &str) -> String {
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        ctx.with(|qctx| {
            inject(&qctx);
            qctx.eval::<String, _>(expr).expect("eval")
        })
    }

    #[test]
    fn unwrap_forms_yield_plain_strings() {
        assert_eq!(run(r#"text("Ac-Me").lower().value"#), "ac-me");
        assert_eq!(run(r#"String(text("Ac-Me").lower())"#), "ac-me");
        assert_eq!(run(r#"JSON.stringify(text("Ac-Me").lower())"#), "\"ac-me\"");
    }

    #[test]
    fn transforms_do_not_mutate_the_receiver() {
        // strip returns a new value; the original is unchanged.
        assert_eq!(
            run(
                r#"(function(){var t=text("  Hi  ");var s=t.strip().value;return s+"|"+t.value;})()"#
            ),
            "Hi|  Hi  "
        );
    }

    #[test]
    fn case_and_strip_renames() {
        assert_eq!(run(r#"text("  Héllo  ").strip().upper().value"#), "HÉLLO");
        assert_eq!(run(r#"text("HeLLo").swap_case().value"#), "hEllO");
        assert_eq!(run(r#"text("mixED cAse").title().value"#), "Mixed Case");
        assert_eq!(run(r#"text("hello").capitalize().value"#), "Hello");
        // strip with an explicit character set.
        assert_eq!(run(r#"text("xxabcxx").strip("x").value"#), "abc");
    }

    #[test]
    fn prefix_suffix_and_predicates() {
        assert_eq!(
            run(r#"String(text("SKU-0042").starts_with("SKU-"))"#),
            "true"
        );
        assert_eq!(
            run(r#"text("SKU-0042").removeprefix("SKU-").value"#),
            "0042"
        );
        assert_eq!(
            run(r#"text("file.txt").removesuffix(".txt").value"#),
            "file"
        );
        assert_eq!(run(r#"String(text("0042").is_digit())"#), "true");
        assert_eq!(run(r#"String(text("Café").is_alpha())"#), "true");
        assert_eq!(run(r#"String(text("A1").is_alnum())"#), "true");
        assert_eq!(run(r#"String(text("").is_digit())"#), "false");
        assert_eq!(run(r#"String(text("  ").is_space())"#), "true");
    }

    #[test]
    fn replace_and_count() {
        // replace hits ALL occurrences (Python semantics), unlike native String.replace.
        assert_eq!(run(r#"text("a.b.c").replace(".", "-").value"#), "a-b-c");
        assert_eq!(run(r#"String(text("a.b.c").count("."))"#), "2");
    }

    #[test]
    fn splitting_returns_plain_strings() {
        assert_eq!(
            run(r#"JSON.stringify(text("a,b,c").split(","))"#),
            r#"["a","b","c"]"#
        );
        assert_eq!(
            run(r#"JSON.stringify(text("a\nb").splitlines())"#),
            r#"["a","b"]"#
        );
        // maxsplit keeps the remainder in the last piece (Python str.split).
        assert_eq!(
            run(r#"JSON.stringify(text("a,b,c").split(",",1))"#),
            r#"["a","b,c"]"#
        );
        assert_eq!(
            run(r#"JSON.stringify(text("a,b,c").rsplit(",",1))"#),
            r#"["a,b","c"]"#
        );
    }

    #[test]
    fn padding_and_alignment() {
        assert_eq!(run(r#"text("42").zfill(6).value"#), "000042");
        assert_eq!(run(r#"text("-42").zfill(6).value"#), "-00042");
        assert_eq!(run(r#"text("x").rjust(5).value"#), "    x");
        assert_eq!(run(r#"text("x").ljust(3, ".").value"#), "x..");
        assert_eq!(run(r#"text("x").center(5, "-").value"#), "--x--");
    }

    #[test]
    fn oversize_width_is_refused() {
        // width beyond the output cap throws rather than allocating unboundedly.
        assert_eq!(
            run(
                r#"(function(){try{text("x").rjust(2e9);return "no-throw";}catch(e){return "threw";}})()"#
            ),
            "threw"
        );
    }

    #[test]
    fn erp_verbs() {
        assert_eq!(
            run(r#"text("  Café Ör 01! ").slugify().value"#),
            "cafe-or-01"
        );
        assert_eq!(
            run(r#"text("4111111111111234").mask().value"#),
            "************1234"
        );
        assert_eq!(
            run(r##"text("4111111111111234").mask({keep:4,char:"#"}).value"##),
            "############1234"
        );
        assert_eq!(run(r#"text("a   b\t c").collapse().value"#), "a b c");
        assert_eq!(run(r#"text("hello world").truncate(8).value"#), "hello w…");
        // no truncation when it already fits.
        assert_eq!(run(r#"text("short").truncate(10).value"#), "short");
    }

    #[test]
    fn present_and_identical_under_the_deterministic_sanitizer() {
        // Eval the sanitizer after injecting text, then confirm the whole surface still works —
        // text touches no ambient authority, so the deterministic profile removes nothing from it.
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        let out = ctx.with(|qctx| {
            inject(&qctx);
            qctx.eval::<(), _>(DETERMINISM).expect("eval determinism");
            qctx.eval::<String, _>(
                r#"text("  Héllo  ").strip().slugify().mask({keep:2}).value + "|" + String(typeof text)"#,
            )
            .expect("eval")
        });
        assert_eq!(out, "***lo|function");
    }
}
