//! The always-on runtime standard library for the sandbox, under `$std`.
//!
//! Pure, zero-I/O helpers grouped at `$std.crypto` (hashing, HMAC, UUID, encoding), plus the
//! operator surfaces `$std.env` / `$std.secrets`. Pure like `$`/`$std.decimal` — crypto is always
//! injected, no config, no per-op metering.
//!
//! Date/time is **not** here: it lives in the first-class top-level `$std.datetime`
//! value-util (`js/datetime.js`), whose calendar/timezone math is served by the
//! `datetime` domain in this same `__sys` bridge (see the `-- datetime` section
//! below). The crypto surface is grouped under `$std.crypto`.
//!
//! FFI: every op crosses the boundary as `__sys(domain, op, payload_json)` and
//! returns `{"v": <result>}` on success or `{"error": <message>}` on failure
//! (mirroring `decimal.rs`). Errors throw a plain JS `Error` in the wrapper, so
//! they classify as developer/script errors — no `__runlet` capability tag is
//! needed (these helpers do no I/O).
//!
//! ## Secrets are use-not-extract — the hard multi-tenant guarantee
//!
//! Tenants run arbitrary JS, so a secret must be **usable** without being
//! **extractable**. The guarantee is structural, not heuristic:
//!
//! 1. **Plaintext never enters the JS heap.** Secret values stay in this Rust
//!    module's per-request [`SecretStore`]; the JS side receives only **opaque
//!    handles** carrying the secret's *name* (see `js/sys.js`). There is no JS
//!    value a script can coerce, slice, or stringify back into the plaintext —
//!    every coercion (`String`, template, `JSON.stringify`, `valueOf`) yields
//!    `"[secret:NAME]"`.
//! 2. **The only op that resolves a handle is HMAC, in the key position.** A
//!    handle reaches plaintext solely via `crypto.hmac({key_ref})`, whose output
//!    is a one-way digest. No native op returns, encodes, or echoes plaintext;
//!    encode/hash/url ops reject handles outright (in JS).
//!
//! That makes extraction impossible *by construction*: there is no decode path to
//! filter, because the plaintext is never present in any JS value to begin with. (We
//! deliberately keep **no** output-redaction fallback — a scan only catches
//! un-transformed values, so it would be evadable security theater, not a guarantee.)
//!
//! *Honest caveat:* HMAC of a **low-entropy** secret is offline-brute-forceable
//! by anyone who can call `hmac` — inherent to HMAC, not a leak here. Secrets
//! must be high-entropy.

use std::collections::HashMap;
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{
    DateTime, Datelike, Days, Months, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Timelike,
    Utc,
};
use chrono_tz::Tz;
use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rquickjs::{Ctx, Function, Object, Value as JsValue};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

use crate::sandbox;

/// Per-request runtime-stdlib context: operator-supplied env + secrets (both opt-in).
///
/// `env` values are returnable plain config; `secrets` plaintext never reaches JS —
/// it stays Rust-side in the [`SecretStore`] and surfaces only as opaque handles.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SysConfig {
    /// Plain config values, exposed (and returnable) at `$std.env`.
    #[serde(default)]
    pub env: Map<String, Value>,
    /// Secret values: plaintext kept Rust-side; `$std.secrets` exposes opaque handles.
    #[serde(default)]
    pub secrets: Map<String, Value>,
}

impl SysConfig {
    /// Per-request name→plaintext map, kept Rust-side and resolved only by HMAC.
    /// Non-string secrets are skipped — a credential is always a string.
    fn secret_store(&self) -> SecretStore {
        self.secrets
            .iter()
            .filter_map(|(name, val)| val.as_str().map(|plain| (name.clone(), plain.to_owned())))
            .collect()
    }

    /// The configured secret names (string-valued only) — the only thing the JS
    /// side learns; it builds an opaque handle per name (no plaintext crosses).
    fn secret_names(&self) -> Vec<&str> {
        self.secrets
            .iter()
            .filter(|(_name, val)| val.is_string())
            .map(|(name, _val)| name.as_str())
            .collect()
    }
}

/// Per-request secret plaintext, indexed by name. Lives only in Rust (captured by
/// the `__sys` native closure); the JS side never sees a value, only opaque handles.
type SecretStore = HashMap<String, String>;

/// JS wrapper — loaded from `src/js/sys.js` at compile time.
pub(crate) const SYS_WRAPPER: &str = include_str!("js/sys.js");

/// Milliseconds in one second.
const MILLIS_PER_SECOND: i64 = 1000;
/// Seconds in one minute.
const SECONDS_PER_MINUTE: u64 = 60;
/// Seconds in one hour.
const SECONDS_PER_HOUR: u64 = 3600;
/// Seconds in one day.
const SECONDS_PER_DAY: u64 = 86_400;

/// Registers the eager `__sys` FFI bridge (the cheap native half), capturing the secret plaintext
/// store. The `$std.crypto`/`env`/`secrets` wrapper is built lazily by the engine on first access;
/// this only installs the bridge those members ride (D2). The secret plaintext lives in the closure
/// here and is resolved only by HMAC — it never crosses into JS (see module docs).
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_native(
    qctx: &Ctx<'_>,
    sys_config: Option<&SysConfig>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let secrets = sys_config.map(SysConfig::secret_store).unwrap_or_default();

    let sys_fn = Function::new(
        qctx.clone(),
        move |domain: String, op: String, payload: String| -> String {
            match dispatch(&domain, &op, &payload, &secrets) {
                Ok(value) => value_json(&value),
                Err(err) => sandbox::error_json(&err),
            }
        },
    )?
    .with_name("__sys")?;

    qctx.globals().set("__sys", sys_fn)?;
    Ok(())
}

/// Builds the JS post-step that populates `$std.env`/`$std.secrets` from `cfg`, to run **inside**
/// the lazy sys build (after `SYS_WRAPPER` defines `__sysMakeSecrets`, with `$std` bound to the
/// build scratch). `env` is embedded as its JSON literal (trusted operator config); `secrets` is
/// built from names only via the wrapper's handle factory (no plaintext crosses into JS). Returns
/// the empty string when there is no config (the wrapper's `{}` defaults then stand).
///
/// # Errors
///
/// Returns an error if serializing the env map / secret names fails.
pub(crate) fn context_post_step(cfg: &SysConfig) -> Result<String, Box<dyn Error + Send + Sync>> {
    let env_json = serde_json::to_string(&cfg.env)?;
    let names_json = serde_json::to_string(&cfg.secret_names())?;
    Ok(format!(
        "$std.env = {env_json}; $std.secrets = globalThis.__sysMakeSecrets({names_json});"
    ))
}

/// Injects the runtime stdlib under `$std` eagerly (native bridge + wrapper + context).
///
/// Retained for the module's own unit tests; the engine registers the native eagerly and builds the
/// wrapper lazily instead. Populates `$std.crypto` (always on, pure), plus `$std.env`/`$std.secrets`
/// only when `sys_config` is present (opt-in).
///
/// # Errors
///
/// Returns an error if registration or JS eval fails.
pub fn inject_sys(
    qctx: &Ctx<'_>,
    sys_config: Option<&SysConfig>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    register_native(qctx, sys_config)?;

    let wrapper: JsValue<'_> = qctx.eval(SYS_WRAPPER)?;
    drop(wrapper);

    if let Some(cfg) = sys_config {
        inject_context(qctx, cfg)?;
    }
    Ok(())
}

/// Populates `$std.env` (plain, returnable values) and `$std.secrets` (opaque
/// handles) from operator config. Crucially, **no secret plaintext is set on any
/// JS value** — only the secret *names* are sent, and the JS wrapper turns each
/// into a frozen handle whose plaintext is reachable solely via Rust-side HMAC.
fn inject_context(qctx: &Ctx<'_>, cfg: &SysConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let std_obj: Object<'_> = qctx.globals().get("$std")?;
    let env_val: JsValue<'_> = qctx.json_parse(serde_json::to_string(&cfg.env)?)?;
    std_obj.set("env", env_val)?;

    // Build the handle map from names only — `__sysMakeSecrets` is defined by the
    // wrapper and returns a frozen object of opaque handles (no plaintext).
    let names_json = serde_json::to_string(&cfg.secret_names())?;
    let snippet = format!("$std.secrets = globalThis.__sysMakeSecrets({names_json});");
    let built: JsValue<'_> = qctx.eval(snippet)?;
    drop(built);
    Ok(())
}

// -- Dispatch ---------------------------------------------------------------

/// Routes a `__sys` call to the right domain handler.
fn dispatch(domain: &str, op: &str, payload: &str, secrets: &SecretStore) -> Result<Value, String> {
    let parsed: Value =
        serde_json::from_str(payload).map_err(|err| format!("invalid payload: {err}"))?;
    match domain {
        "crypto" => crypto_dispatch(op, &parsed, secrets),
        "datetime" => datetime_dispatch(op, &parsed),
        other => Err(format!("unknown sys domain: '{other}'")),
    }
}

/// Wraps a result value in the `{"v": ...}` success envelope.
fn value_json(value: &Value) -> String {
    match serde_json::to_string(value) {
        Ok(inner) => format!("{{\"v\":{inner}}}"),
        Err(_err) => "{\"error\":\"failed to encode result\"}".to_owned(),
    }
}

// -- Payload helpers --------------------------------------------------------

/// Reads a required string field from the payload object.
fn field_str<'a>(payload: &'a Value, key: &str) -> Result<&'a str, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field '{key}'"))
}

/// Reads a required integer field from the payload object.
fn field_i64(payload: &Value, key: &str) -> Result<i64, String> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing integer field '{key}'"))
}

// -- crypto -----------------------------------------------------------------

/// Routes a `$std.crypto` op.
fn crypto_dispatch(op: &str, payload: &Value, secrets: &SecretStore) -> Result<Value, String> {
    match op {
        "sha256" => Ok(Value::String(sha256_hex(field_str(payload, "data")?))),
        "sha512" => Ok(Value::String(sha512_hex(field_str(payload, "data")?))),
        "hmac" => hmac_op(payload, secrets),
        "uuid" => Ok(Value::String(Uuid::now_v7().to_string())),
        "base64_encode" | "base64_decode" | "base64url_encode" | "base64url_decode"
        | "hex_encode" | "hex_decode" | "url_encode" | "url_decode" => {
            encoding_dispatch(op, payload)
        }
        other => Err(format!("unknown crypto op: '{other}'")),
    }
}

/// SHA-256 of a UTF-8 string, hex-encoded.
fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

/// SHA-512 of a UTF-8 string, hex-encoded.
fn sha512_hex(data: &str) -> String {
    let mut hasher = Sha512::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

/// HMAC over `{algo, key|key_ref, msg, encoding}` → encoded digest.
///
/// The key is either a script-supplied `key` string or a `key_ref` naming an
/// operator secret resolved from [`SecretStore`] — the *only* path by which secret
/// plaintext is read, and a one-way one (the output is a digest, never the key).
fn hmac_op(payload: &Value, secrets: &SecretStore) -> Result<Value, String> {
    let algo = field_str(payload, "algo")?;
    let key = resolve_key(payload, secrets)?;
    let msg = field_str(payload, "msg")?;
    let encoding = payload
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("hex");
    let bytes = match algo {
        "sha256" => hmac_sha256(key.as_bytes(), msg.as_bytes())?,
        "sha512" => hmac_sha512(key.as_bytes(), msg.as_bytes())?,
        other => return Err(format!("unsupported hmac algorithm: '{other}'")),
    };
    Ok(Value::String(encode_bytes(&bytes, encoding)?))
}

/// Resolves the HMAC key: a `key_ref` looks up secret plaintext (Rust-only), else a
/// plain `key` string is used. The resolved plaintext is returned to the caller for
/// immediate one-way hashing and never serialized back across the FFI boundary.
fn resolve_key(payload: &Value, secrets: &SecretStore) -> Result<String, String> {
    if let Some(name) = payload.get("key_ref").and_then(Value::as_str) {
        return secrets
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown secret: '{name}'"));
    }
    Ok(field_str(payload, "key")?.to_owned())
}

/// HMAC-SHA-256 raw bytes.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|err| format!("hmac key error: {err}"))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// HMAC-SHA-512 raw bytes.
fn hmac_sha512(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac =
        Hmac::<Sha512>::new_from_slice(key).map_err(|err| format!("hmac key error: {err}"))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Encodes raw bytes as `hex` / `base64` / `base64url`.
fn encode_bytes(bytes: &[u8], encoding: &str) -> Result<String, String> {
    match encoding {
        "hex" => Ok(hex::encode(bytes)),
        "base64" => Ok(STANDARD.encode(bytes)),
        "base64url" => Ok(URL_SAFE_NO_PAD.encode(bytes)),
        other => Err(format!("unsupported encoding: '{other}'")),
    }
}

/// Routes the encode/decode ops (`base64`/`base64url`/`hex`/`url`).
fn encoding_dispatch(op: &str, payload: &Value) -> Result<Value, String> {
    let data = field_str(payload, "data")?;
    match op {
        "base64_encode" => Ok(Value::String(STANDARD.encode(data.as_bytes()))),
        "base64_decode" => decode_utf8(&base64_decode(&STANDARD, data)?),
        "base64url_encode" => Ok(Value::String(URL_SAFE_NO_PAD.encode(data.as_bytes()))),
        "base64url_decode" => decode_utf8(&base64_decode(&URL_SAFE_NO_PAD, data)?),
        "hex_encode" => Ok(Value::String(hex::encode(data.as_bytes()))),
        "hex_decode" => {
            decode_utf8(&hex::decode(data).map_err(|err| format!("invalid hex: {err}"))?)
        }
        "url_encode" => Ok(Value::String(
            utf8_percent_encode(data, NON_ALPHANUMERIC).to_string(),
        )),
        "url_decode" => url_decode(data),
        other => Err(format!("unknown encoding op: '{other}'")),
    }
}

/// Decodes a base64 string with the given engine.
fn base64_decode<E: Engine>(engine: &E, data: &str) -> Result<Vec<u8>, String> {
    engine
        .decode(data)
        .map_err(|err| format!("invalid base64: {err}"))
}

/// Turns decoded bytes back into a UTF-8 string, erroring on non-text input.
fn decode_utf8(bytes: &[u8]) -> Result<Value, String> {
    String::from_utf8(bytes.to_vec())
        .map(Value::String)
        .map_err(|err| format!("decoded bytes are not valid utf-8: {err}"))
}

/// Percent-decodes a string back to UTF-8.
fn url_decode(data: &str) -> Result<Value, String> {
    percent_decode_str(data)
        .decode_utf8()
        .map(|decoded| Value::String(decoded.into_owned()))
        .map_err(|err| format!("invalid percent-encoding: {err}"))
}

// -- datetime ---------------------------------------------------------------
//
// Serves the top-level `datetime` value-util (`js/datetime.js`). The JS side holds the immutable
// value (a UTC epoch-millis instant + an optional IANA zone for a *view*); this domain does the
// chrono/chrono-tz calendar math. Ops take `ms` (the canonical instant) and, where the answer is
// zone-dependent (components, boundaries, formatting), an optional IANA `zone` (UTC when absent).
// The canonical instant is never changed by a zone — only the interpretation is.

/// Routes a `datetime` op.
fn datetime_dispatch(op: &str, payload: &Value) -> Result<Value, String> {
    match op {
        "now" => Ok(Value::from(now_millis()?)),
        "parse" => datetime_parse(payload),
        "from" => datetime_from(payload),
        "add" => datetime_add(payload),
        "parts" => datetime_parts(payload),
        "start_of" => datetime_boundary(payload, false),
        "end_of" => datetime_boundary(payload, true),
        "iso" => datetime_iso(payload),
        "format" => datetime_format(payload),
        "diff" => datetime_diff(payload),
        other => Err(format!("unknown datetime op: '{other}'")),
    }
}

/// Current wall-clock instant as epoch milliseconds. The **only** ambient-clock reader in this
/// domain — removed (not stubbed) under `Profile::Deterministic`: the lazy `datetime` builder
/// constructs the variant with `datetime.now` deleted (see `engine::build_unit_sources`).
fn now_millis() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock error: {err}"))?;
    i64::try_from(elapsed.as_millis()).map_err(|err| format!("timestamp overflow: {err}"))
}

/// Parses the `input` field (ISO string, date-only, or epoch millis) → epoch millis.
fn datetime_parse(payload: &Value) -> Result<Value, String> {
    let input = payload
        .get("input")
        .ok_or_else(|| "missing 'input'".to_owned())?;
    Ok(Value::from(parse_input(input)?))
}

/// Resolves a JSON value (string or number) to epoch milliseconds.
fn parse_input(input: &Value) -> Result<i64, String> {
    match input {
        Value::Number(num) => num
            .as_i64()
            .ok_or_else(|| "epoch millis must be an integer".to_owned()),
        Value::String(text) => parse_date_str(text),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
            Err("datetime input must be an ISO string or epoch millis".to_owned())
        }
    }
}

/// Parses an RFC 3339 timestamp or a date-only `YYYY-MM-DD` → epoch milliseconds (UTC).
/// Locale-formatted strings (e.g. `07/10/2026`) are deliberately **not** guessed — they fail here.
fn parse_date_str(text: &str) -> Result<i64, String> {
    let trimmed = text.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(parsed.timestamp_millis());
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| "invalid date".to_owned())?;
        return Ok(naive.and_utc().timestamp_millis());
    }
    Err(format!("cannot parse date: '{text}'"))
}

/// Builds an instant from `{parts:{year,month,day,hour?,minute?,second?,millisecond?}, zone?}`,
/// interpreting the parts in `zone` (UTC when absent) → epoch milliseconds.
fn datetime_from(payload: &Value) -> Result<Value, String> {
    let parts = payload
        .get("parts")
        .ok_or_else(|| "missing 'parts'".to_owned())?;
    let year = i32::try_from(part_i64(parts, "year", 0)?)
        .map_err(|_err| "year out of range".to_owned())?;
    let month = part_u32(parts, "month", 1)?;
    let day = part_u32(parts, "day", 1)?;
    let hour = part_u32(parts, "hour", 0)?;
    let minute = part_u32(parts, "minute", 0)?;
    let second = part_u32(parts, "second", 0)?;
    let milli = part_u32(parts, "millisecond", 0)?;
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|date| date.and_hms_milli_opt(hour, minute, second, milli))
        .ok_or_else(|| "date parts are out of range".to_owned())?;
    let tz = resolve_zone(payload)?;
    Ok(Value::from(from_local(tz, naive)?))
}

/// Reads an optional integer `key` from a parts object, defaulting when absent.
fn part_i64(parts: &Value, key: &str, default: i64) -> Result<i64, String> {
    parts.get(key).map_or(Ok(default), |val| {
        val.as_i64()
            .ok_or_else(|| format!("'{key}' must be an integer"))
    })
}

/// Reads an optional non-negative integer `key` as a `u32`, defaulting when absent.
fn part_u32(parts: &Value, key: &str, default: u32) -> Result<u32, String> {
    parts.get(key).map_or(Ok(default), |val| {
        val.as_i64()
            .and_then(|num| u32::try_from(num).ok())
            .ok_or_else(|| format!("'{key}' must be a non-negative integer"))
    })
}

/// Shifts a `ms` instant by `months` (calendar months, end-of-month-clamped) then `fixed_ms`
/// (fixed-length units the JS side already summed and signed). Overflow → error.
///
/// `months` carries years too (`years*12 + months`, signed); calendar shift runs first (larger
/// units), then the fixed offset — matching Luxon/Temporal ordering. Chrono's `Months` addition
/// clamps to the last valid day (Jan 31 + 1 month → Feb 28/29).
fn datetime_add(payload: &Value) -> Result<Value, String> {
    let base = instant(field_i64(payload, "ms")?)?;
    let months = field_i64(payload, "months")?;
    let fixed_ms = field_i64(payload, "fixed_ms")?;
    let count = u32::try_from(months.unsigned_abs())
        .map_err(|_err| "month arithmetic overflow".to_owned())?;
    let shifted = if months < 0 {
        base.checked_sub_months(Months::new(count))
    } else {
        base.checked_add_months(Months::new(count))
    }
    .ok_or_else(|| "datetime arithmetic overflow".to_owned())?;
    let out = shifted
        .timestamp_millis()
        .checked_add(fixed_ms)
        .ok_or_else(|| "datetime arithmetic overflow".to_owned())?;
    // Confirm the result stays representable so downstream formatting can't fail.
    instant(out).map(|_dt| ())?;
    Ok(Value::from(out))
}

/// Returns the full calendar-component bundle for a `ms` instant, resolved in the optional `zone`
/// (UTC when absent): components, ISO weekday/week, quarter, day-of-year, days-in-month.
fn datetime_parts(payload: &Value) -> Result<Value, String> {
    let tz = resolve_zone(payload)?;
    let zoned = instant(field_i64(payload, "ms")?)?.with_timezone(&tz);
    let (year, month) = (zoned.year(), zoned.month());
    let iso = zoned.iso_week();
    let quarter = month
        .checked_sub(1)
        .and_then(|zero_based| zero_based.checked_div(3))
        .and_then(|qtr| qtr.checked_add(1))
        .ok_or_else(|| "quarter computation overflow".to_owned())?;
    Ok(serde_json::json!({
        "year": year,
        "month": month,
        "day": zoned.day(),
        "hour": zoned.hour(),
        "minute": zoned.minute(),
        "second": zoned.second(),
        "millisecond": zoned.timestamp_subsec_millis(),
        "weekday": zoned.weekday().number_from_monday(),
        "quarter": quarter,
        "day_of_year": zoned.ordinal(),
        "iso_week": { "week": iso.week(), "week_year": iso.year() },
        "days_in_month": days_in_month(year, month)?,
    }))
}

/// Returns the period boundary (`start`/`end`) for `{ms, unit, zone?}` as epoch millis, computed
/// in the optional `zone` (UTC when absent). `unit` ∈ `day|week|month|quarter|year`.
fn datetime_boundary(payload: &Value, end: bool) -> Result<Value, String> {
    let unit = field_str(payload, "unit")?;
    let tz = resolve_zone(payload)?;
    let local = instant(field_i64(payload, "ms")?)?
        .with_timezone(&tz)
        .naive_local()
        .date();
    let target = boundary_date(local, unit, end)?;
    let naive = if end {
        target.and_hms_milli_opt(23, 59, 59, 999)
    } else {
        target.and_hms_milli_opt(0, 0, 0, 0)
    }
    .ok_or_else(|| "invalid boundary time".to_owned())?;
    Ok(Value::from(from_local(tz, naive)?))
}

/// The boundary calendar date for `unit` (start, or end when `end`), given the local date.
fn boundary_date(date: NaiveDate, unit: &str, end: bool) -> Result<NaiveDate, String> {
    match unit {
        "day" => Ok(date),
        "week" => week_boundary(date, end),
        "month" => month_boundary(date, end),
        "quarter" => quarter_boundary(date, end),
        "year" => year_boundary(date, end),
        other => Err(format!("unknown boundary unit: '{other}'")),
    }
}

/// Monday of `date`'s ISO week, or the following Sunday when `end`.
fn week_boundary(date: NaiveDate, end: bool) -> Result<NaiveDate, String> {
    let back = u64::from(date.weekday().num_days_from_monday());
    let monday = date
        .checked_sub_days(Days::new(back))
        .ok_or_else(|| "week boundary out of range".to_owned())?;
    if end {
        monday
            .checked_add_days(Days::new(6))
            .ok_or_else(|| "week boundary out of range".to_owned())
    } else {
        Ok(monday)
    }
}

/// The first (or last, when `end`) day of `date`'s month.
fn month_boundary(date: NaiveDate, end: bool) -> Result<NaiveDate, String> {
    let day = if end {
        days_in_month(date.year(), date.month())?
    } else {
        1
    };
    date.with_day(day)
        .ok_or_else(|| "month boundary out of range".to_owned())
}

/// The first (or last, when `end`) day of `date`'s calendar quarter.
fn quarter_boundary(date: NaiveDate, end: bool) -> Result<NaiveDate, String> {
    let zero_based = date
        .month()
        .checked_sub(1)
        .ok_or_else(|| "quarter boundary out of range".to_owned())?;
    let first_of_quarter = zero_based
        .checked_div(3)
        .and_then(|qtr| qtr.checked_mul(3))
        .and_then(|mon| mon.checked_add(1))
        .ok_or_else(|| "quarter boundary out of range".to_owned())?;
    let month = if end {
        first_of_quarter
            .checked_add(2)
            .ok_or_else(|| "quarter boundary out of range".to_owned())?
    } else {
        first_of_quarter
    };
    let anchored = date
        .with_day(1)
        .and_then(|first| first.with_month(month))
        .ok_or_else(|| "quarter boundary out of range".to_owned())?;
    if end {
        month_boundary(anchored, true)
    } else {
        Ok(anchored)
    }
}

/// January 1 (or December 31, when `end`) of `date`'s year.
fn year_boundary(date: NaiveDate, end: bool) -> Result<NaiveDate, String> {
    let (month, day) = if end { (12, 31) } else { (1, 1) };
    date.with_month(month)
        .and_then(|anchored| anchored.with_day(day))
        .ok_or_else(|| "year boundary out of range".to_owned())
}

/// Formats a `ms` instant as an RFC 3339 string — `Z` in UTC (no `zone`), else the zone's offset.
fn datetime_iso(payload: &Value) -> Result<Value, String> {
    let instant = instant(field_i64(payload, "ms")?)?;
    match payload.get("zone").and_then(Value::as_str) {
        None => Ok(Value::String(
            instant.to_rfc3339_opts(SecondsFormat::AutoSi, true),
        )),
        Some(name) => {
            let tz = parse_zone(name)?;
            Ok(Value::String(
                instant
                    .with_timezone(&tz)
                    .to_rfc3339_opts(SecondsFormat::AutoSi, false),
            ))
        }
    }
}

/// Formats `{ms, pattern, zone?}` with a locale-neutral numeric token dialect (`YYYY MM DD HH mm ss
/// SSS YY`). Any other character is a literal — no locale-language month/day names are produced.
fn datetime_format(payload: &Value) -> Result<Value, String> {
    let tz = resolve_zone(payload)?;
    let zoned = instant(field_i64(payload, "ms")?)?.with_timezone(&tz);
    let pattern = field_str(payload, "pattern")?;
    Ok(Value::String(render_pattern(pattern, &zoned)))
}

/// Substitutes the numeric field tokens in `pattern` with zero-padded values from `zoned`,
/// greedily matching the longest token first; unmatched characters pass through verbatim.
fn render_pattern<T: TimeZone>(pattern: &str, zoned: &DateTime<T>) -> String {
    let year = zoned.year();
    let two_digit_year = year.rem_euclid(100);
    let tokens: [(&str, String); 8] = [
        ("YYYY", format!("{year:04}")),
        ("SSS", format!("{:03}", zoned.timestamp_subsec_millis())),
        ("YY", format!("{two_digit_year:02}")),
        ("MM", format!("{:02}", zoned.month())),
        ("DD", format!("{:02}", zoned.day())),
        ("HH", format!("{:02}", zoned.hour())),
        ("mm", format!("{:02}", zoned.minute())),
        ("ss", format!("{:02}", zoned.second())),
    ];
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    'scan: while !rest.is_empty() {
        for (token, value) in &tokens {
            if let Some(tail) = rest.strip_prefix(token) {
                out.push_str(value);
                rest = tail;
                continue 'scan;
            }
        }
        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            out.push(ch);
            rest = chars.as_str();
        }
    }
    out
}

/// Computes `a - b` and breaks the gap into days/hours/minutes/seconds.
fn datetime_diff(payload: &Value) -> Result<Value, String> {
    let first = field_i64(payload, "a")?;
    let second = field_i64(payload, "b")?;
    let total_ms = first
        .checked_sub(second)
        .ok_or_else(|| "datetime diff overflow".to_owned())?;
    let total_seconds = total_ms
        .checked_div(MILLIS_PER_SECOND)
        .ok_or_else(|| "timestamp overflow".to_owned())?;
    let (days, hours, minutes, seconds) = split_duration(total_seconds.unsigned_abs())?;
    Ok(serde_json::json!({
        "total_ms": total_ms,
        "total_seconds": total_seconds,
        "days": days,
        "hours": hours,
        "minutes": minutes,
        "seconds": seconds,
    }))
}

/// Reconstructs a `DateTime<Utc>` from epoch millis, erroring if out of range.
fn instant(ms: i64) -> Result<DateTime<Utc>, String> {
    DateTime::<Utc>::from_timestamp_millis(ms).ok_or_else(|| "timestamp out of range".to_owned())
}

/// Resolves the optional IANA `zone` field to a timezone (UTC when absent). Unknown name → error.
fn resolve_zone(payload: &Value) -> Result<Tz, String> {
    payload
        .get("zone")
        .and_then(Value::as_str)
        .map_or(Ok(Tz::UTC), parse_zone)
}

/// Parses an IANA timezone name; an unrecognized name is a developer error.
fn parse_zone(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .map_err(|_err| format!("unknown timezone: '{name}'"))
}

/// Localizes a naive datetime into `tz` and returns its epoch millis, taking the earliest instant
/// for a fold (DST fall-back) and erroring on a gap (a local time that does not exist).
fn from_local(tz: Tz, naive: NaiveDateTime) -> Result<i64, String> {
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp_millis())
        .ok_or_else(|| "local time does not exist in the given zone".to_owned())
}

/// The number of days in the given calendar month (handles leap Februaries).
fn days_in_month(year: i32, month: u32) -> Result<u32, String> {
    let (next_year, next_month) = if month >= 12 {
        (
            year.checked_add(1)
                .ok_or_else(|| "year out of range".to_owned())?,
            1,
        )
    } else {
        (
            year,
            month
                .checked_add(1)
                .ok_or_else(|| "month out of range".to_owned())?,
        )
    };
    let first_of_next = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| "invalid date".to_owned())?;
    Ok(first_of_next
        .pred_opt()
        .ok_or_else(|| "date out of range".to_owned())?
        .day())
}

/// Breaks an absolute second count into (days, hours, minutes, seconds).
fn split_duration(total_seconds: u64) -> Result<(u64, u64, u64, u64), String> {
    let (days, after_days) = divmod(total_seconds, SECONDS_PER_DAY)?;
    let (hours, after_hours) = divmod(after_days, SECONDS_PER_HOUR)?;
    let (minutes, seconds) = divmod(after_hours, SECONDS_PER_MINUTE)?;
    Ok((days, hours, minutes, seconds))
}

/// Checked `(value / unit, value % unit)`.
fn divmod(value: u64, unit: u64) -> Result<(u64, u64), String> {
    let quot = value
        .checked_div(unit)
        .ok_or_else(|| "duration overflow".to_owned())?;
    let rem = value
        .checked_rem(unit)
        .ok_or_else(|| "duration overflow".to_owned())?;
    Ok((quot, rem))
}
