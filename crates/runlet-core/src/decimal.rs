//! Exact decimal helper for the `QuickJS` sandbox (`Decimal`) and the numeric core the
//! currency-bound `money` (`$`) wrapper composes over.
//!
//! Backed by `rust_decimal` — the same engine used to decode `NUMERIC` columns over the wire, so
//! in-script math matches the database exactly. Always injected (it is pure: no I/O, no config, no
//! per-op metering).
//!
//! JS API: `Decimal(value)` builds a decimal; methods `add/sub/mul/div/neg/abs/round/round_to/cmp/…`.
//! Every op crosses the FFI boundary as strings: `__decimal(op, lhs, rhs, aux)` returns a
//! `{"v":"<value>"}` scalar envelope, an `{"list":[…]}` array envelope (`allocate`), or
//! `{"error":"…"}`. The currency table lives JS-side (`money.js`); Rust only ever receives a
//! resolved `places` integer, so currency-awareness costs zero Rust.

use std::cmp::Ordering;
use std::error::Error;
use std::str::FromStr;

use rquickjs::{Ctx, Function, Value as JsValue};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::sandbox;

/// JS wrapper — loaded from `src/js/decimal.js` at compile time. Built lazily on first
/// `$std.decimal` access by the engine's lazy-`$std` path (see `engine::inject_lazy_std`).
pub(crate) const DECIMAL_WRAPPER: &str = include_str!("js/decimal.js");

/// Registers the eager `__decimal` FFI bridge (the cheap native half). The expensive `$std.decimal`
/// wrapper is built lazily by the engine on first access; this only installs the bridge the wrapper
/// composes over, so it must be present up front (D2).
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_native(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let decimal_fn = Function::new(
        qctx.clone(),
        |op: String, lhs: String, rhs: String, aux: String| -> String {
            match dispatch(&op, &lhs, &rhs, &aux) {
                Ok(out) => out.into_json(),
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__decimal")?;

    qctx.globals().set("__decimal", decimal_fn)?;
    Ok(())
}

/// Injects `$std.decimal` eagerly (native bridge + wrapper). Retained for the module's own unit
/// tests; the engine now registers the native eagerly and builds the wrapper lazily instead.
///
/// The currency-bound `$` / `money` global is injected separately (see `money.rs`) and composes
/// over the same `__decimal` FFI.
///
/// # Errors
///
/// Returns an error if registration or JS eval fails.
pub fn inject_decimal(qctx: &Ctx<'_>) -> Result<(), Box<dyn Error + Send + Sync>> {
    register_native(qctx)?;
    let wrapper: JsValue<'_> = qctx.eval(DECIMAL_WRAPPER)?;
    drop(wrapper);
    Ok(())
}

// -- Dispatch ---------------------------------------------------------------

/// The result of a `__decimal` op: either one scalar or an ordered list (`allocate`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecimalOut {
    /// A single decimal string — the `{"v":…}` envelope.
    Scalar(String),
    /// An ordered list of decimal strings — the `{"list":[…]}` envelope.
    List(Vec<String>),
}

impl DecimalOut {
    /// Serializes to the wire envelope the JS wrapper unwraps.
    fn into_json(self) -> String {
        match self {
            Self::Scalar(value) => value_json(&value),
            Self::List(values) => list_json(&values),
        }
    }
}

/// Routes a `__decimal` call to the right operation. `rhs`/`aux` carry per-op auxiliary arguments
/// (a second operand, a `places` count, a rounding `mode`, a `step`, or a weights array).
fn dispatch(op: &str, lhs: &str, rhs: &str, aux: &str) -> Result<DecimalOut, String> {
    let left = parse_decimal(lhs)?;
    match op {
        "parse" => Ok(DecimalOut::Scalar(left.to_string())),
        "neg" => negate(left).map(DecimalOut::Scalar),
        "abs" => Ok(DecimalOut::Scalar(left.abs().to_string())),
        "round" => round(left, rhs, aux).map(DecimalOut::Scalar),
        "round_to" => round_to(left, rhs, aux).map(DecimalOut::Scalar),
        "to_cents" | "to_minor" => to_minor(left, rhs).map(DecimalOut::Scalar),
        "from_cents" | "from_minor" => from_minor(left, rhs).map(DecimalOut::Scalar),
        "cmp" => compare(left, rhs).map(DecimalOut::Scalar),
        "allocate" => allocate(left, rhs, aux).map(DecimalOut::List),
        "add" | "sub" | "mul" | "div" => arithmetic(op, left, rhs).map(DecimalOut::Scalar),
        other => Err(format!("unknown decimal op: {other}")),
    }
}

// -- Rounding-mode vocabulary -----------------------------------------------

/// Maps the `snake_case` rounding-mode vocabulary onto `rust_decimal`'s strategy, adopting the
/// established Java `RoundingMode` meaning. An empty `mode` defaults to `half_up` (commercial
/// rounding, backward-compatible with the prior `round`); an unrecognized mode is a catchable error.
fn rounding_strategy(mode: &str) -> Result<RoundingStrategy, String> {
    match mode.trim() {
        "" | "half_up" => Ok(RoundingStrategy::MidpointAwayFromZero),
        "half_even" => Ok(RoundingStrategy::MidpointNearestEven),
        "up" => Ok(RoundingStrategy::AwayFromZero),
        "down" => Ok(RoundingStrategy::ToZero),
        "ceil" => Ok(RoundingStrategy::ToPositiveInfinity),
        "floor" => Ok(RoundingStrategy::ToNegativeInfinity),
        other => Err(format!("unknown rounding mode: '{other}'")),
    }
}

// -- Operations -------------------------------------------------------------

/// Parses a decimal string, trimming surrounding whitespace.
fn parse_decimal(value: &str) -> Result<Decimal, String> {
    Decimal::from_str(value.trim()).map_err(|err| format!("invalid decimal '{value}': {err}"))
}

/// Negates a decimal via `0 - x` (cannot overflow for `Decimal`'s symmetric range).
fn negate(value: Decimal) -> Result<String, String> {
    Decimal::ZERO
        .checked_sub(value)
        .map(|out| out.to_string())
        .ok_or_else(|| "decimal overflow".to_owned())
}

/// Parses a non-negative decimal-places count (shared by `round`/`to_minor`/`from_minor`).
fn parse_places(places_str: &str) -> Result<u32, String> {
    places_str
        .trim()
        .parse()
        .map_err(|_err| format!("invalid places: '{places_str}'"))
}

/// Rounds to `places` decimal places using the `mode` strategy (default `half_up`).
fn round(value: Decimal, places_str: &str, mode: &str) -> Result<String, String> {
    let places = parse_places(places_str)?;
    let strategy = rounding_strategy(mode)?;
    Ok(value.round_dp_with_strategy(places, strategy).to_string())
}

/// Rounds to the nearest multiple of `step` (e.g. `"0.05"` cash rounding) using `mode`.
fn round_to(value: Decimal, step_str: &str, mode: &str) -> Result<String, String> {
    let step = parse_decimal(step_str)?;
    if step <= Decimal::ZERO {
        return Err(format!("round_to step must be positive: '{step_str}'"));
    }
    let strategy = rounding_strategy(mode)?;
    let quotient = value
        .checked_div(step)
        .ok_or_else(|| "decimal overflow".to_owned())?
        .round_dp_with_strategy(0, strategy);
    quotient
        .checked_mul(step)
        .map(|out| out.to_string())
        .ok_or_else(|| "decimal overflow".to_owned())
}

/// Parses the minor-unit exponent (fraction digits, e.g. 2 for cents), defaulting to 2.
fn minor_places(places_str: &str) -> Result<u32, String> {
    let trimmed = places_str.trim();
    if trimmed.is_empty() {
        return Ok(2);
    }
    let places = parse_places(trimmed)?;
    if places > 18 {
        return Err(format!("minor-unit places too large: {places} (max 18)"));
    }
    Ok(places)
}

/// Computes `10^places` as a `Decimal` (the major↔minor scale factor).
fn scale_factor(places: u32) -> Result<Decimal, String> {
    10_u64
        .checked_pow(places)
        .map(Decimal::from)
        .ok_or_else(|| "minor-unit scale overflow".to_owned())
}

/// Converts major units to minor units: `value * 10^places`, rounded half-up to an integer.
fn to_minor(value: Decimal, places_str: &str) -> Result<String, String> {
    let factor = scale_factor(minor_places(places_str)?)?;
    let scaled = value
        .checked_mul(factor)
        .ok_or_else(|| "decimal overflow".to_owned())?;
    Ok(scaled
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_string())
}

/// Converts minor units to major units: `value / 10^places`, fixed to `places` decimals.
fn from_minor(value: Decimal, places_str: &str) -> Result<String, String> {
    let places = minor_places(places_str)?;
    let major = value
        .checked_div(scale_factor(places)?)
        .ok_or_else(|| "decimal overflow".to_owned())?;
    Ok(major
        .round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero)
        .to_string())
}

/// Compares two decimals, returning `"-1"`, `"0"`, or `"1"`.
fn compare(left: Decimal, rhs: &str) -> Result<String, String> {
    let right = parse_decimal(rhs)?;
    let token = match left.cmp(&right) {
        Ordering::Less => "-1",
        Ordering::Equal => "0",
        Ordering::Greater => "1",
    };
    Ok(token.to_owned())
}

/// Performs a checked binary operation (`add`/`sub`/`mul`/`div`).
fn arithmetic(op: &str, left: Decimal, rhs: &str) -> Result<String, String> {
    let right = parse_decimal(rhs)?;
    let out = match op {
        "add" => left.checked_add(right),
        "sub" => left.checked_sub(right),
        "mul" => left.checked_mul(right),
        "div" => {
            if right == Decimal::ZERO {
                return Err("division by zero".to_owned());
            }
            left.checked_div(right)
        }
        other => return Err(format!("unknown decimal op: {other}")),
    };
    out.map(|value| value.to_string())
        .ok_or_else(|| "decimal overflow".to_owned())
}

// -- Allocation (largest-remainder / Hamilton) ------------------------------

/// Splits `value` into shares proportional to `weights_json` (a JSON array of numbers/strings) so
/// the shares sum to `value` **exactly** at the currency's minor unit (`places`): floor each share
/// to a minor unit, then hand out the leftover minor units to the largest fractional remainders,
/// breaking ties by input order (deterministic). Returns the shares as major-unit strings.
fn allocate(value: Decimal, places_str: &str, weights_json: &str) -> Result<Vec<String>, String> {
    let places = minor_places(places_str)?;
    let scale = scale_factor(places)?;
    let total_minor = value
        .checked_mul(scale)
        .ok_or_else(|| "decimal overflow".to_owned())?
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);

    let weights = parse_weights(weights_json)?;
    let sum_weights = sum_decimals(&weights)?;
    if sum_weights <= Decimal::ZERO {
        return Err("allocation weights must sum to a positive value".to_owned());
    }

    let (mut shares, remainders) = floor_shares(total_minor, &weights, sum_weights)?;
    distribute_leftover(total_minor, &mut shares, &remainders)?;
    shares
        .iter()
        .map(|minor| minor_to_major(*minor, scale, places))
        .collect()
}

/// Parses the weights array (each element a JSON number or numeric string) into decimals.
fn parse_weights(weights_json: &str) -> Result<Vec<Decimal>, String> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(weights_json)
        .map_err(|err| format!("invalid allocation weights: {err}"))?;
    if raw.is_empty() {
        return Err("allocation needs at least one weight".to_owned());
    }
    raw.iter().map(weight_to_decimal).collect()
}

/// Coerces one JSON weight (number or string) to a non-negative decimal.
fn weight_to_decimal(value: &serde_json::Value) -> Result<Decimal, String> {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    let weight = parse_decimal(&text)?;
    if weight < Decimal::ZERO {
        return Err(format!("allocation weight cannot be negative: '{text}'"));
    }
    Ok(weight)
}

/// Sums a slice of decimals with checked addition.
fn sum_decimals(values: &[Decimal]) -> Result<Decimal, String> {
    values.iter().try_fold(Decimal::ZERO, |acc, value| {
        acc.checked_add(*value)
            .ok_or_else(|| "decimal overflow".to_owned())
    })
}

/// Floors each proportional share to whole minor units, returning the floors and their fractional
/// remainders (aligned by index) for leftover distribution.
fn floor_shares(
    total_minor: Decimal,
    weights: &[Decimal],
    sum_weights: Decimal,
) -> Result<(Vec<Decimal>, Vec<Decimal>), String> {
    let mut shares = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    for weight in weights {
        let exact = total_minor
            .checked_mul(*weight)
            .and_then(|scaled| scaled.checked_div(sum_weights))
            .ok_or_else(|| "decimal overflow".to_owned())?;
        let floored = exact.floor();
        let remainder = exact
            .checked_sub(floored)
            .ok_or_else(|| "decimal overflow".to_owned())?;
        shares.push(floored);
        remainders.push(remainder);
    }
    Ok((shares, remainders))
}

/// Hands the leftover minor units (`total_minor - Σshares`) one at a time to the shares with the
/// largest remainders, breaking ties by ascending index (order-stable, deterministic).
fn distribute_leftover(
    total_minor: Decimal,
    shares: &mut [Decimal],
    remainders: &[Decimal],
) -> Result<(), String> {
    let allocated = sum_decimals(shares)?;
    let mut leftover = total_minor
        .checked_sub(allocated)
        .ok_or_else(|| "decimal overflow".to_owned())?;

    // (index, remainder) pairs sorted by remainder desc, ties by ascending index (deterministic).
    let mut order: Vec<(usize, Decimal)> = remainders.iter().copied().enumerate().collect();
    order.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    for (index, _remainder) in order {
        if leftover <= Decimal::ZERO {
            break;
        }
        if let Some(share) = shares.get_mut(index) {
            *share = share
                .checked_add(Decimal::ONE)
                .ok_or_else(|| "decimal overflow".to_owned())?;
            leftover = leftover
                .checked_sub(Decimal::ONE)
                .ok_or_else(|| "decimal overflow".to_owned())?;
        }
    }
    Ok(())
}

/// Converts a whole-minor-unit share back to a fixed-`places` major-unit string. `rescale` pads to
/// exactly `places` decimals so shares read uniformly (`"5.00"`, not `"5"`); the value is already
/// exact at the minor unit, so no rounding occurs.
fn minor_to_major(minor: Decimal, scale: Decimal, places: u32) -> Result<String, String> {
    let mut major = minor
        .checked_div(scale)
        .ok_or_else(|| "decimal overflow".to_owned())?;
    major.rescale(places);
    Ok(major.to_string())
}

// -- Output -----------------------------------------------------------------

/// Builds the success envelope `{"v":"<value>"}`.
fn value_json(value: &str) -> String {
    let escaped = serde_json::to_string(value).unwrap_or_else(|_err| "\"0\"".to_owned());
    format!("{{\"v\":{escaped}}}")
}

/// Builds the list envelope `{"list":["a","b",…]}` (the `allocate` shape).
fn list_json(values: &[String]) -> String {
    let escaped = serde_json::to_string(values).unwrap_or_else(|_err| "[]".to_owned());
    format!("{{\"list\":{escaped}}}")
}

#[cfg(test)]
mod tests {
    use super::{DecimalOut, dispatch};

    /// Convenience: run a scalar op and unwrap the `Scalar` string.
    fn scalar(op: &str, lhs: &str, rhs: &str, aux: &str) -> Result<String, String> {
        match dispatch(op, lhs, rhs, aux)? {
            DecimalOut::Scalar(value) => Ok(value),
            DecimalOut::List(_list) => Err("expected scalar".to_owned()),
        }
    }

    /// Convenience: run `allocate` and unwrap the `List`.
    fn list(lhs: &str, places: &str, weights: &str) -> Result<Vec<String>, String> {
        match dispatch("allocate", lhs, places, weights)? {
            DecimalOut::List(values) => Ok(values),
            DecimalOut::Scalar(_value) => Err("expected list".to_owned()),
        }
    }

    #[test]
    fn round_default_mode_is_half_up() {
        assert_eq!(scalar("round", "19.985", "2", ""), Ok("19.99".to_owned()));
        assert_eq!(scalar("round", "2.5", "0", ""), Ok("3".to_owned()));
    }

    #[test]
    fn round_half_even_is_bankers() {
        assert_eq!(scalar("round", "2.5", "0", "half_even"), Ok("2".to_owned()));
        assert_eq!(scalar("round", "3.5", "0", "half_even"), Ok("4".to_owned()));
    }

    #[test]
    fn round_directed_modes() {
        assert_eq!(scalar("round", "1.1", "0", "up"), Ok("2".to_owned()));
        assert_eq!(scalar("round", "1.9", "0", "down"), Ok("1".to_owned()));
        assert_eq!(scalar("round", "-1.1", "0", "floor"), Ok("-2".to_owned()));
        assert_eq!(scalar("round", "-1.9", "0", "ceil"), Ok("-1".to_owned()));
    }

    #[test]
    fn round_unknown_mode_errors() {
        assert!(scalar("round", "1.5", "0", "sideways").is_err());
    }

    #[test]
    fn round_to_nearest_step() {
        assert_eq!(
            scalar("round_to", "2.03", "0.05", ""),
            Ok("2.05".to_owned())
        );
        assert_eq!(
            scalar("round_to", "2.02", "0.05", ""),
            Ok("2.00".to_owned())
        );
    }

    #[test]
    fn to_minor_scales_by_currency_places() {
        assert_eq!(scalar("to_minor", "19.99", "2", ""), Ok("1999".to_owned()));
        assert_eq!(scalar("to_minor", "1000", "0", ""), Ok("1000".to_owned()));
        assert_eq!(scalar("to_minor", "1.234", "3", ""), Ok("1234".to_owned()));
    }

    #[test]
    fn allocate_equal_split_preserves_total() {
        assert_eq!(
            list("100.00", "2", "[1,1,1]"),
            Ok(vec![
                "33.34".to_owned(),
                "33.33".to_owned(),
                "33.33".to_owned()
            ])
        );
    }

    #[test]
    fn allocate_weighted_split_leftover_to_earlier_larger_remainder() {
        assert_eq!(
            list("0.05", "2", "[70,30]"),
            Ok(vec!["0.04".to_owned(), "0.01".to_owned()])
        );
    }

    #[test]
    fn allocate_zero_decimal_currency_whole_units() {
        assert_eq!(
            list("1000", "0", "[1,1,1]"),
            Ok(vec!["334".to_owned(), "333".to_owned(), "333".to_owned()])
        );
    }

    #[test]
    fn allocate_is_deterministic() {
        let first = list("100.00", "2", "[1,1,1]");
        let second = list("100.00", "2", "[1,1,1]");
        assert_eq!(first, second);
    }

    #[test]
    fn allocate_preserves_share_count_with_zero_weight() {
        assert_eq!(
            list("10.00", "2", "[1,0,1]"),
            Ok(vec![
                "5.00".to_owned(),
                "0.00".to_owned(),
                "5.00".to_owned()
            ])
        );
    }

    #[test]
    fn allocate_all_zero_weights_errors() {
        assert!(list("10.00", "2", "[0,0]").is_err());
    }

    #[test]
    fn from_minor_default_places_fixes_two_decimals() {
        assert_eq!(scalar("from_minor", "1999", "", ""), Ok("19.99".to_owned()));
        assert_eq!(scalar("from_cents", "150", "", ""), Ok("1.50".to_owned()));
    }

    #[test]
    fn division_by_zero_errors() {
        assert!(scalar("div", "10", "0", "").is_err());
    }
}
