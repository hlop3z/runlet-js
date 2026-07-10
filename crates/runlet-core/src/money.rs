//! Currency-bound money helper for the `QuickJS` sandbox (`$` / `money`).
//!
//! `$` and `money` are the same constructor: `$(amount, currency?)` builds a currency-bound value
//! that is safe by construction (same-currency arithmetic, no implicit FX, per-currency minor
//! units). It is pure JS composed over the `__decimal` FFI and the ISO 4217 exponent table in
//! `money.js` — no Rust FFI of its own — so it is always injected, right after `Decimal`.
//!
//! The one datum crossing from Rust is the resolved **default currency** (`config.currency` else
//! the operator `default_currency`), set as `globalThis.__default_currency` for the construction
//! cascade to fall back on. `None`/empty leaves the fallback unset, so a currency-less `$("19.99")`
//! throws a plain-language error.

use std::error::Error;

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/money.js` at compile time. Depends on `Decimal` already being
/// injected (it reads `globalThis.Decimal`).
const MONEY_WRAPPER: &str = include_str!("js/money.js");

/// Injects the `$` / `money` global. Must run after [`crate::decimal::inject_decimal`].
///
/// `default_currency` is the resolved cascade fallback (`config.currency` else operator default);
/// `None` leaves `__default_currency` empty so an unspecified-currency construction throws.
///
/// # Errors
///
/// Returns an error if setting the global or JS eval fails.
pub fn inject_money(
    qctx: &Ctx<'_>,
    default_currency: Option<&str>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    qctx.globals()
        .set("__default_currency", default_currency.unwrap_or(""))?;
    let wrapper: JsValue<'_> = qctx.eval(MONEY_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
