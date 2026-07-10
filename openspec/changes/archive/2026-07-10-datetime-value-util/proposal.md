## Why

The sandbox exposes date/time handling under `$sys.date` — a minimal UTC-only surface (`now/parse/add/sub/diff/iso/unix`) that predates both the snake_case business-scripting convention (it still has `epochMs`) and the polished value-util tier (`$`/`money`/`Decimal`). Business/ERP scripts routinely need calendar components, period boundaries, month/year arithmetic, and above all **timezone-correct** answers ("end of month in the customer's timezone") that a UTC-only surface structurally cannot give. This change promotes date/time to a first-class value-util, `datetime`, sitting beside `money`/`Decimal`, and enriches it to cover real ERP needs.

## What Changes

- **New top-level `datetime` global** (factory model mirroring `money`): `datetime.now()`, `datetime.parse(input)`, `datetime(input)`, `datetime.from({...}, zone?)`. Returns an immutable UTC-instant value with chainable, snake_case methods.
- **Enriched surface (Tiers 1–5):**
  - **Components** — `year/month/day/hour/minute/second/millisecond`, `weekday` (ISO 1=Mon…7=Sun), `quarter`, `day_of_year`, `iso_week`, `days_in_month`.
  - **Period boundaries** — `start_of(unit)` / `end_of(unit)` for `day|week|month|quarter|year` (week start ISO-Monday by default).
  - **Calendar arithmetic** — `add`/`sub` gain `{months, years}` (end-of-month clamping: Jan 31 + 1mo → Feb 28/29), plus `diff`/`diff_in(unit)` and weekend-aware `add_business_days`/`is_weekend`/`is_business_day`.
  - **Comparison** — `cmp/eq/lt/lte/gt/gte/is_between` (mirrors `Decimal`/`Money`).
  - **Timezone (i18n core)** — `in_zone("America/New_York")` returns a zoned *view* whose components/boundaries/formatting resolve in that zone; the canonical value stays a UTC instant.
- **Formatting** — `iso(zone?)`, `unix`, `epoch_ms`, `format(pattern, zone?)` (strftime numeric tokens, locale-**neutral**).
- **snake_case throughout** — `epoch_ms` replaces `epochMs`.
- **Determinism preserved** — `datetime.now()` is *removed* (not stubbed) under `Profile::Deterministic`, exactly as `$sys.date.now` is today; no comparison helper reads the ambient clock.
- **BREAKING — `$sys.date` removed.** Hard move: the surface exists only as `datetime`. Migrates the determinism neutralization hook, engine comments, and `docs/09-sys.md` accordingly. (Near-zero real entanglement: no test/registered scripts use `$sys.date`.)
- **Out of scope (explicitly):** locale-*language* names (deferred; if ever added, via chrono `unstable-locales`, not ICU); holiday/business-day calendars (country/company-specific).

## Capabilities

### New Capabilities
- `datetime`: the first-class `datetime` value-util — construction/parsing, components, period boundaries, calendar arithmetic, comparison, timezone-aware views, and numeric formatting; always-on, determinism-neutralized `now()`.

### Modified Capabilities
- `sys`: remove the `$sys.date` requirements — the "Pure helpers always injected" requirement drops `$sys.date` (crypto stays), and the "Date helpers" requirement is removed. Date/time behavior moves wholesale to the new `datetime` capability.
- `capability-registry`: the deterministic-profile mux-bypass requirement names the neutralized wall clock as "`$sys` clock"; update that wording to point at the `datetime` clock (entropy stays under `$sys.crypto`). Wording-only; the removed-not-stubbed invariant is unchanged.

## Impact

- **New dependency:** `chrono-tz` (IANA tz database) for timezone views — one mature adopt, embeds tzdata (binary-size cost), a new cargo-vet subject. This is the sole `/opsx:decide` build-vs-adopt concern.
- **Code:** `crates/runlet-core/src/js/` (new `datetime.js`, remove date from `sys.js`), `sys.rs` date domain moves/extends into a datetime domain, `js/base.d.ts` (new `datetime` declarations, remove `SysDate`; D11 golden test `types_dts_is_up_to_date` must stay green), `js/determinism.js` (retarget `now()` deletion), `engine.rs` comments.
- **Docs:** `docs/09-sys.md` (drop date section), a new capability doc for `datetime`, `README.md` reference sync.
- **No change** to `runlet` (HTTP front), the wire contract, or any capability config — `datetime` is always-on and unmetered, like `money`/`Decimal`.
