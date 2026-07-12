use super::{ExecOutcome, ExecParams, Profile, run};
use rquickjs::Runtime;
use std::time::Duration;

/// Runs `script` and returns the success `data`/`error` envelope JSON. `default_currency` seeds
/// the construction cascade fallback (the resolved `config.currency` else operator default).
fn run_script(script: &str, default_currency: Option<&str>) -> String {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let params = ExecParams {
        runtime: &runtime,
        bytecode_cache: None,
        cache_namespace: None,
        script,
        context_json: "{}",
        timeout: Duration::from_secs(5),
        profile: Profile::Full,
        sys_config: None,
        registry: None,
        enabled_io: &[],
        egress: None,
        default_currency,
        max_ops: 4096,
        max_emit_kind_len: 64,
        log_level: super::LogLevel::Info,
        max_log_entries: 256,
        max_log_entry_bytes: 256 * 1024,
        max_log_total_bytes: 1024 * 1024,
        max_output_size: 0,
        allow_private_targets: false,
    };
    let result = run(&params).unwrap_or_else(|_err| unreachable!());
    match result.outcome {
        ExecOutcome::Success(json) => json,
        ExecOutcome::Error(err) => format!("ERROR: {err:?}"),
    }
}

/// `Decimal` is exact and distinct from `$`; snake_case resolves and the removed camelCase
/// alias is absent (calling it throws a `TypeError`, caught here as `false`).
#[test]
fn decimal_is_exact_and_distinct_from_money() {
    let out = run_script(
        "function handler() { return json({ \
               sum: $std.decimal('0.1').add('0.2').toString(), \
               distinct: $std.decimal !== $, \
               snake: $std.decimal('0').is_zero(), \
               camel_gone: (typeof $std.decimal('0').isZero !== 'function') }); }",
        None,
    );
    assert!(out.contains("\"sum\":\"0.3\""), "exact base-10: {out}");
    assert!(out.contains("\"distinct\":true"), "Decimal !== $: {out}");
    assert!(
        out.contains("\"snake\":true") && out.contains("\"camel_gone\":true"),
        "snake_case resolves and camelCase alias removed: {out}"
    );
}

/// `Decimal` bounded helpers + banker's rounding + cash rounding compose in JS.
#[test]
fn decimal_helpers_and_modes() {
    let out = run_script(
        "function handler() { return json({ \
               clamp: $std.decimal('120').clamp(0, 100).toString(), \
               bankers: $std.decimal('2.5').round(0, 'half_even').toString(), \
               cash: $std.decimal('2.03').round_to('0.05').toString() }); }",
        None,
    );
    assert!(out.contains("\"clamp\":\"100\""), "{out}");
    assert!(
        out.contains("\"bankers\":\"2\""),
        "half_even ties to even: {out}"
    );
    assert!(out.contains("\"cash\":\"2.05\""), "{out}");
}

/// An invoice-with-tax flow: percentages round to the currency precision, `$`/`money` alias.
#[test]
fn money_invoice_with_tax() {
    let out = run_script(
        "function handler() { \
               const net = $('100.00', 'USD'); \
               const gross = net.add_pct(8.25); \
               return json({ gross: gross.to_string(), alias: $std.money === $, \
                 minor: gross.to_minor(), fmt: gross.format() }); }",
        None,
    );
    assert!(out.contains("\"gross\":\"108.25\""), "{out}");
    assert!(out.contains("\"alias\":true"), "{out}");
    assert!(out.contains("\"minor\":10825"), "{out}");
    assert!(out.contains("\"fmt\":\"$108.25\""), "{out}");
}

/// A refund split: penny-safe allocation whose shares sum to the total exactly.
#[test]
fn money_refund_split_is_penny_safe() {
    let out = run_script(
        "function handler() { \
               const parts = $('100.00', 'USD').allocate_to(3).map(function (m) { return m.to_string(); }); \
               return json({ parts: parts }); }",
        None,
    );
    assert!(
        out.contains("[\"33.34\",\"33.33\",\"33.33\"]"),
        "equal split sums to 100.00: {out}"
    );
}

/// `div` is overloaded: money ÷ scalar → money, money ÷ same-currency money → a Decimal ratio.
#[test]
fn money_div_overload() {
    let out = run_script(
        "function handler() { \
               const unit = $('99.00', 'USD').div(3).to_string(); \
               const ratio = $('115.00', 'USD').div($('100.00', 'USD')).toString(); \
               return json({ unit: unit, ratio: ratio }); }",
        None,
    );
    // Money arithmetic stays exact (rounding is explicit): 99.00 / 3 keeps the 2-place scale.
    assert!(out.contains("\"unit\":\"33.00\""), "{out}");
    assert!(
        out.contains("\"ratio\":\"1.15\""),
        "money/money ratio: {out}"
    );
}

/// Currency safety: cross-currency add throws a catchable error (no implicit FX).
#[test]
fn money_cross_currency_add_throws() {
    let out = run_script(
        "function handler() { \
               try { $('1.00','USD').add($('1.00','EUR')); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
        None,
    );
    assert!(out.contains("\"caught\":true"), "{out}");
}

/// The currency cascade: no explicit arg falls back to the resolved default currency.
#[test]
fn money_currency_cascade_uses_default() {
    let out = run_script(
        "function handler() { return json($('19.99')); }",
        Some("EUR"),
    );
    assert!(out.contains("\"currency\":\"EUR\""), "{out}");
    assert!(out.contains("\"minor_units\":1999"), "{out}");
}

/// No currency resolvable → a catchable construction error.
#[test]
fn money_no_currency_throws() {
    let out = run_script(
        "function handler() { \
               try { $('19.99'); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
        None,
    );
    assert!(out.contains("\"caught\":true"), "{out}");
}

/// Self-describing serialization: JPY (exponent 0) has integer `minor_units`, no ×100.
#[test]
fn money_serializes_self_describing_zero_decimal() {
    let out = run_script(
        "function handler() { return json({ y: $('1000', 'JPY') }); }",
        None,
    );
    assert!(out.contains("\"amount\":\"1000\""), "{out}");
    assert!(out.contains("\"currency\":\"JPY\""), "{out}");
    assert!(out.contains("\"minor_units\":1000"), "not 100000: {out}");
}

/// The ISO 4217 exponent table drives precision: BHD=3, CLF=4 minor units; an unknown code throws.
#[test]
fn money_currency_exponent_table() {
    let bhd = run_script(
        "function handler() { return json($('1.234', 'BHD')); }",
        None,
    );
    assert!(
        bhd.contains("\"minor_units\":1234"),
        "BHD exponent 3: {bhd}"
    );
    let clf = run_script(
        "function handler() { return json($('1.2345', 'CLF')); }",
        None,
    );
    assert!(
        clf.contains("\"minor_units\":12345"),
        "CLF exponent 4: {clf}"
    );
    let bad = run_script(
        "function handler() { \
               try { $('10', 'ZZZ'); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
        None,
    );
    assert!(
        bad.contains("\"caught\":true"),
        "unknown currency throws: {bad}"
    );
}
