//! The first-class `check` value-util for the `QuickJS` sandbox.
//!
//! `check` is the always-on checksum-verification value-util at `$std.check` (beside
//! `$std.text`/`$std.template`): a callable factory (`$std.check(value)`) that wraps a value and
//! exposes scheme methods — `luhn()` (ISO/IEC 7812-1), `gtin()` (ISO/IEC 15420), and
//! `iso7064(system)` (ISO/IEC 7064, v1: the MOD 97-10 system) — each returning a boolean asserting
//! that the value's check digit is internally consistent (never that the entity is real or
//! registered).
//!
//! Pure JS (`js/check.js`) — no `__sys` bridge and no Rust math: every scheme is small integer
//! arithmetic over `Number` (MOD 97-10 via a piecewise modulus, so no `BigInt`), so this injector
//! only evals the wrapper. It is injected under **both** profiles: `check` touches no clock, no
//! randomness, and no ambient authority, so the deterministic sanitizer (`js/determinism.js`)
//! removes nothing from it. Registry/jurisdiction validators (`iban`/`bic`/`vat`) and
//! publishing-only schemes (`isbn`/`issn`) are deliberate permanent non-goals.

use std::error::Error;

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/check.js` at compile time. Depends on no other injected global
/// besides the `$std` bootstrap object.
const CHECK_WRAPPER: &str = include_str!("js/check.js");

/// Injects `$std.check`. Order-independent among the pure value-utils (no bridge dependency); only
/// requires that the `$std` bootstrap object already exists.
///
/// # Errors
///
/// Returns an error if the JS eval fails.
pub fn inject_check(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let wrapper: JsValue<'_> = qctx.eval(CHECK_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Behavioral tests for the `check` value-util, driven end-to-end through the QuickJS engine
    //! (the surface is pure JS, so there is no Rust `dispatch` to unit-test directly). Each test
    //! injects the wrapper and evals an assertion expression against `$std.check`.

    use rquickjs::{Context, Runtime};

    use super::inject_check;

    /// The deterministic-profile sanitizer, evaled to prove `check` survives it untouched.
    const DETERMINISM: &str = include_str!("js/determinism.js");

    /// Bootstraps `$std` (the wrapper populates it, not `globalThis`) then injects `check`. Unlike
    /// `text`, `check` is namespace-only, so nothing is projected to a bare global — tests call
    /// `$std.check(...)` directly.
    fn inject(qctx: &rquickjs::Ctx<'_>) {
        qctx.eval::<(), _>("globalThis.$std = {};")
            .expect("bootstrap std");
        inject_check(qctx).expect("inject check");
    }

    /// Inject `check` and eval a JS expression, returning its string form (booleans via `String`).
    fn run(expr: &str) -> String {
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        ctx.with(|qctx| {
            inject(&qctx);
            qctx.eval::<String, _>(expr).expect("eval")
        })
    }

    #[test]
    fn luhn_valid_and_invalid() {
        // Standards golden: the canonical Luhn worked example and its off-by-one neighbour.
        assert_eq!(run(r#"String($std.check("79927398713").luhn())"#), "true");
        assert_eq!(run(r#"String($std.check("79927398714").luhn())"#), "false");
        // A well-known valid test card number.
        assert_eq!(
            run(r#"String($std.check("4111111111111111").luhn())"#),
            "true"
        );
    }

    #[test]
    fn luhn_tolerates_space_and_hyphen_formatting() {
        assert_eq!(
            run(r#"String($std.check("4111 1111 1111 1111").luhn())"#),
            "true"
        );
        assert_eq!(
            run(r#"String($std.check("4111-1111-1111-1111").luhn())"#),
            "true"
        );
    }

    #[test]
    fn gtin_valid_ean13_and_upca() {
        // EAN-13 (GTIN-13) and UPC-A (GTIN-12) standards goldens.
        assert_eq!(run(r#"String($std.check("4006381333931").gtin())"#), "true");
        assert_eq!(run(r#"String($std.check("036000291452").gtin())"#), "true");
    }

    #[test]
    fn gtin_wrong_digit_and_unsupported_length() {
        assert_eq!(
            run(r#"String($std.check("4006381333932").gtin())"#),
            "false"
        );
        // 5 digits is not a GTIN length.
        assert_eq!(run(r#"String($std.check("12345").gtin())"#), "false");
    }

    #[test]
    fn iso7064_mod97_10_valid_and_corrupted() {
        // Standards golden: the GB82 IBAN rearranged (country + check moved to the end) reduces to
        // 1 mod 97. Hand-verified via the piecewise modulus.
        assert_eq!(
            run(r#"String($std.check("WEST12345698765432GB82").iso7064("mod_97_10"))"#),
            "true"
        );
        assert_eq!(
            run(r#"String($std.check("WEST12345698765433GB82").iso7064("mod_97_10"))"#),
            "false"
        );
    }

    #[test]
    fn iso7064_unknown_system_is_false_not_error() {
        assert_eq!(
            run(r#"String($std.check("123").iso7064("mod_999"))"#),
            "false"
        );
    }

    #[test]
    fn malformed_input_returns_false_never_throws() {
        // Empty, non-digit, and out-of-alphabet inputs return false rather than throwing.
        assert_eq!(run(r#"String($std.check("").luhn())"#), "false");
        assert_eq!(run(r#"String($std.check("12x4").luhn())"#), "false");
        assert_eq!(run(r#"String($std.check("123").gtin())"#), "false");
        assert_eq!(
            run(r#"String($std.check("!!").iso7064("mod_97_10"))"#),
            "false"
        );
        // Prove no throw across all three schemes on junk input.
        assert_eq!(
            run(
                r#"(function(){try{$std.check("$#@").luhn();$std.check("$#@").gtin();$std.check("$#@").iso7064("mod_97_10");return "no-throw";}catch(e){return "threw";}})()"#
            ),
            "no-throw"
        );
    }

    #[test]
    fn no_branded_jurisdictional_or_publishing_methods() {
        // The permanent non-goals must not exist on the value (D6).
        assert_eq!(
            run(
                r#"(function(){var c=$std.check("x");return [c.iban,c.bic,c.vat,c.isbn,c.issn].every(function(m){return m===undefined;})?"none":"present";})()"#
            ),
            "none"
        );
        // And there is no bare `check` global — it is reached only through `$std`.
        assert_eq!(run(r#"String(typeof globalThis.check)"#), "undefined");
    }

    #[test]
    fn present_and_identical_under_the_deterministic_sanitizer() {
        // Eval the sanitizer after injecting check, then confirm the whole surface still works —
        // check touches no ambient authority, so the deterministic profile removes nothing from it.
        let rt = Runtime::new().expect("runtime");
        let ctx = Context::full(&rt).expect("context");
        let out = ctx.with(|qctx| {
            inject(&qctx);
            qctx.eval::<(), _>(DETERMINISM).expect("eval determinism");
            qctx.eval::<String, _>(
                r#"String(typeof $std.check) + "|" + String($std.check("79927398713").luhn()) + "|" + String($std.check("4006381333931").gtin())"#,
            )
            .expect("eval")
        });
        assert_eq!(out, "function|true|true");
    }
}
