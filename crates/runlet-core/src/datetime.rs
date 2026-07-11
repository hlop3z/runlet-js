//! The first-class `datetime` value-util for the `QuickJS` sandbox.
//!
//! `datetime` is the always-on date/time global beside `$`/`money`/`Decimal`: a callable factory
//! (`datetime(input)` ≡ `datetime.parse`) plus named constructors (`datetime.now`,
//! `datetime.parse`, `datetime.from`). A value is an immutable canonical UTC instant with chainable,
//! `snake_case` methods; a zoned *view* (`in_zone`) re-interprets components/boundaries/formatting in
//! an IANA zone without moving the instant.
//!
//! Pure JS (`js/datetime.js`) over the shared `__sys` bridge's `datetime` domain (`sys.rs`) — the
//! calendar/timezone math is Rust (chrono + chrono-tz), so this injector only evals the wrapper.
//! It **must** run after `__sys` is registered. Injected (lazily) under both profiles; under
//! [`crate::engine::Profile::Deterministic`] the lazy builder constructs the variant with only the
//! ambient-clock reader `datetime.now` removed (see `engine::build_unit_sources`).

use std::error::Error;

use rquickjs::{Ctx, Value as JsValue};

/// JS wrapper — loaded from `src/js/datetime.js` at compile time. Depends on the `__sys` bridge
/// (registered by [`crate::sys::inject_sys`]) being present.
pub(crate) const DATETIME_WRAPPER: &str = include_str!("js/datetime.js");

/// Injects the `datetime` global. Must run after [`crate::sys::inject_sys`].
///
/// # Errors
///
/// Returns an error if the JS eval fails.
pub fn inject_datetime(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let wrapper: JsValue<'_> = qctx.eval(DATETIME_WRAPPER)?;
    drop(wrapper);
    Ok(())
}
