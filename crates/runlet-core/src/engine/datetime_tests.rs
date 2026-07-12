use super::{ExecOutcome, ExecParams, Profile, run};
use rquickjs::Runtime;
use std::time::Duration;

/// Runs `script` under `profile` and returns the success `data`/`error` envelope JSON.
fn run_profiled(script: &str, profile: Profile) -> String {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());
    let params = ExecParams {
        runtime: &runtime,
        bytecode_cache: None,
        surface: None,
        cache_namespace: None,
        script,
        context_json: "{}",
        timeout: Duration::from_secs(5),
        profile,
        sys_config: None,
        registry: None,
        enabled_io: &[],
        egress: None,
        default_currency: None,
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

/// Convenience: run under the full profile.
fn run_script(script: &str) -> String {
    run_profiled(script, Profile::Full)
}

/// Parsing normalizes RFC 3339 / date-only / epoch millis / a `datetime`; components read UTC.
#[test]
fn parse_forms_and_components() {
    let out = run_script(
        "function handler() { \
               const d = $std.datetime.parse('2026-07-10T13:30:00Z'); \
               return json({ \
                 year: d.year(), month: d.month(), day: d.day(), \
                 hour: d.hour(), minute: d.minute(), \
                 weekday: d.weekday(), quarter: d.quarter(), \
                 doy: d.day_of_year(), dim: d.days_in_month(), \
                 dateOnly: $std.datetime('2026-07-10').iso(), \
                 fromMs: $std.datetime.parse(d.epoch_ms()).eq(d), \
                 passthrough: $std.datetime(d).eq(d) }); }",
    );
    assert!(out.contains("\"year\":2026"), "{out}");
    assert!(
        out.contains("\"month\":7") && out.contains("\"day\":10"),
        "{out}"
    );
    assert!(
        out.contains("\"hour\":13") && out.contains("\"minute\":30"),
        "{out}"
    );
    assert!(out.contains("\"weekday\":5"), "Friday = ISO 5: {out}");
    assert!(out.contains("\"quarter\":3"), "{out}");
    assert!(out.contains("\"doy\":191"), "day-of-year: {out}");
    assert!(out.contains("\"dim\":31"), "July has 31 days: {out}");
    assert!(
        out.contains("\"dateOnly\":\"2026-07-10T00:00:00Z\""),
        "date-only parses to UTC midnight: {out}"
    );
    assert!(
        out.contains("\"fromMs\":true") && out.contains("\"passthrough\":true"),
        "{out}"
    );
}

/// Ambiguous locale strings are rejected (not guessed); unparseable input throws.
#[test]
fn locale_strings_are_not_guessed() {
    let out = run_script(
        "function handler() { \
               try { $std.datetime.parse('07/10/2026'); return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
    );
    assert!(out.contains("\"caught\":true"), "{out}");
}

/// A `datetime` value is immutable — every op returns a new value, never mutating the receiver.
#[test]
fn value_is_immutable() {
    let out = run_script(
        "function handler() { \
               const d = $std.datetime.parse('2026-07-10T00:00:00Z'); \
               const next = d.add({ days: 1 }); \
               return json({ before: d.day(), after: next.day(), same: d.eq(next) }); }",
    );
    assert!(out.contains("\"before\":10"), "receiver unchanged: {out}");
    assert!(out.contains("\"after\":11"), "{out}");
    assert!(out.contains("\"same\":false"), "{out}");
}

/// Serialization: a value renders as its RFC 3339 UTC (`Z`) string in `json(...)`.
#[test]
fn serializes_as_rfc3339_utc() {
    let out = run_script(
        "function handler() { \
               return json({ d: $std.datetime.parse('2026-07-10T13:30:00Z'), \
                 ms: $std.datetime.parse('2026-07-10T13:30:00Z').epoch_ms() }); }",
    );
    assert!(out.contains("\"d\":\"2026-07-10T13:30:00Z\""), "{out}");
    assert!(out.contains("\"ms\":1783690200000"), "{out}");
}

/// Calendar month arithmetic clamps end-of-month: Jan 31 + 1 month → Feb 28 (2026 non-leap).
#[test]
fn month_arithmetic_clamps_end_of_month() {
    let out = run_script(
        "function handler() { \
               const d = $std.datetime.from({ year: 2026, month: 1, day: 31 }).add({ months: 1 }); \
               return json({ month: d.month(), day: d.day() }); }",
    );
    assert!(
        out.contains("\"month\":2") && out.contains("\"day\":28"),
        "clamped to Feb 28: {out}"
    );
}

/// Arithmetic that overflows the representable range throws rather than wrapping.
#[test]
fn overflow_throws() {
    let out = run_script(
        "function handler() { \
               try { $std.datetime.parse('2026-07-10T00:00:00Z').add({ years: 100000000000 }); \
                 return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
    );
    assert!(out.contains("\"caught\":true"), "{out}");
}

/// Period boundaries: start/end of month land on the first/last instant of the month.
#[test]
fn period_boundaries() {
    let out = run_script(
        "function handler() { \
               const d = $std.datetime.parse('2026-07-15T12:00:00Z'); \
               return json({ start: d.start_of('month').iso(), end: d.end_of('month').iso(), \
                 qstart: d.start_of('quarter').iso(), \
                 wstart_wd: d.start_of('week').weekday() }); }",
    );
    assert!(out.contains("\"start\":\"2026-07-01T00:00:00Z\""), "{out}");
    assert!(
        out.contains("\"end\":\"2026-07-31T23:59:59.999Z\""),
        "{out}"
    );
    assert!(
        out.contains("\"qstart\":\"2026-07-01T00:00:00Z\""),
        "Q3 starts in July: {out}"
    );
    assert!(out.contains("\"wstart_wd\":1"), "week starts Monday: {out}");
}

/// Weekend-aware business days: Friday + 1 business day → Monday; Saturday is a weekend.
#[test]
fn business_day_helpers() {
    let out = run_script(
        "function handler() { \
               const fri = $std.datetime.parse('2026-07-10T00:00:00Z'); \
               const mon = fri.add_business_days(1); \
               const sat = $std.datetime.parse('2026-07-11T00:00:00Z'); \
               return json({ day: mon.day(), wd: mon.weekday(), \
                 satWeekend: sat.is_weekend(), friBusiness: fri.is_business_day() }); }",
    );
    assert!(
        out.contains("\"day\":13") && out.contains("\"wd\":1"),
        "Fri + 1 business day = Mon 13th: {out}"
    );
    assert!(out.contains("\"satWeekend\":true"), "{out}");
    assert!(out.contains("\"friBusiness\":true"), "{out}");
}

/// Difference: structured `diff` and whole-unit `diff_in`, both signed.
#[test]
fn difference() {
    let out = run_script(
        "function handler() { \
               const a = $std.datetime.parse('2026-07-10T00:00:00Z'); \
               const b = $std.datetime.parse('2026-07-08T00:00:00Z'); \
               return json({ days: a.diff(b).days, total: a.diff(b).total_ms, \
                 wholeFwd: a.diff_in(b, 'days'), wholeBack: b.diff_in(a, 'days') }); }",
    );
    assert!(out.contains("\"days\":2"), "{out}");
    assert!(out.contains("\"total\":172800000"), "{out}");
    assert!(
        out.contains("\"wholeFwd\":2") && out.contains("\"wholeBack\":-2"),
        "signed whole days: {out}"
    );
}

/// Timezone view: components resolve in-zone, the canonical instant is preserved, boundaries are
/// computed in the zone, and `iso()` renders the zone offset.
#[test]
fn timezone_view() {
    let out = run_script(
        "function handler() { \
               const d = $std.datetime.parse('2026-07-15T12:00:00Z'); \
               const tokyo = d.in_zone('Asia/Tokyo'); \
               return json({ preserved: tokyo.epoch_ms() === d.epoch_ms(), \
                 hour: tokyo.hour(), day: tokyo.day(), \
                 monthStart: tokyo.start_of('month').iso() }); }",
    );
    assert!(
        out.contains("\"preserved\":true"),
        "instant preserved: {out}"
    );
    assert!(
        out.contains("\"hour\":21"),
        "12:00Z is 21:00 in Tokyo: {out}"
    );
    assert!(out.contains("\"day\":15"), "{out}");
    assert!(
        out.contains("\"monthStart\":\"2026-07-01T00:00:00+09:00\""),
        "month start computed + rendered in Tokyo: {out}"
    );
}

/// An unknown IANA zone name throws.
#[test]
fn unknown_zone_throws() {
    let out = run_script(
        "function handler() { \
               try { $std.datetime.parse('2026-07-10T00:00:00Z').in_zone('Mars/Phobos'); \
                 return json(null, 'no throw'); } \
               catch (e) { return json({ caught: true }); } }",
    );
    assert!(out.contains("\"caught\":true"), "{out}");
}

/// Numeric formatting uses locale-neutral tokens; no locale-language names.
#[test]
fn numeric_formatting() {
    let out = run_script(
        "function handler() { \
               return json({ f: $std.datetime.parse('2026-07-10T13:30:05.250Z')\
                 .format('YYYY-MM-DD HH:mm:ss.SSS') }); }",
    );
    assert!(out.contains("\"f\":\"2026-07-10 13:30:05.250\""), "{out}");
}

/// ISO-week reporting follows ISO-8601 (week-year may differ from the calendar year).
#[test]
fn iso_week_reporting() {
    // 2027-01-01 is a Friday → ISO week 53 of week-year 2026.
    let out = run_script(
        "function handler() { \
               const w = $std.datetime.parse('2027-01-01T00:00:00Z').iso_week(); \
               return json({ week: w.week, week_year: w.week_year }); }",
    );
    assert!(
        out.contains("\"week\":53") && out.contains("\"week_year\":2026"),
        "ISO week-year differs from calendar year: {out}"
    );
}

/// Determinism: `$std.datetime.now` is removed (not stubbed) under the deterministic profile, while
/// parsing/components/arithmetic — pure given an explicit instant — still work.
#[test]
fn deterministic_profile_removes_only_now() {
    let out = run_profiled(
        "function handler() { \
               return json({ noNow: typeof $std.datetime.now === 'undefined', \
                 stillParses: $std.datetime.parse('2026-07-10T00:00:00Z').year(), \
                 arithmetic: $std.datetime.parse('2026-07-10T00:00:00Z').add({ days: 1 }).day() }); }",
        Profile::Deterministic,
    );
    assert!(
        out.contains("\"noNow\":true"),
        "$std.datetime.now removed under deterministic profile: {out}"
    );
    assert!(out.contains("\"stillParses\":2026"), "{out}");
    assert!(out.contains("\"arithmetic\":11"), "{out}");
}

/// Under the full profile `$std.datetime.now` is present and returns a value.
#[test]
fn full_profile_has_now() {
    let out = run_script(
        "function handler() { \
               return json({ hasNow: typeof $std.datetime.now === 'function', \
                 positive: $std.datetime.now().epoch_ms() > 0 }); }",
    );
    assert!(out.contains("\"hasNow\":true"), "{out}");
    assert!(out.contains("\"positive\":true"), "{out}");
}
