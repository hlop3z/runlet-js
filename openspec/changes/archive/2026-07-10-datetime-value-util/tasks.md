## 1. Resolve the build-vs-adopt gate

- [x] 1.1 Run `/opsx:decide` for the `chrono-tz` (IANA tz database) adopt decision; record the ADR block in `design.md` under "Decisions (from /opsx:decide)"
- [x] 1.2 On approval, add `chrono-tz` to the workspace deps (matching chrono's `default-features = false` discipline; pick the smallest feature set that provides IANA zones), and add the cargo-vet exemption/audit entry so `task supply-chain` passes

## 2. Rust datetime domain (`crates/runlet-core/src/sys.rs`)

- [x] 2.1 Extend the existing `date` domain into the full datetime op set: `now`, `parse`, `from`, `add`/`sub` (with `months`/`years` + end-of-month clamping via chrono `Months`, checked overflow → error), `diff`, `diff_in`
- [x] 2.2 Add component ops: `year/month/day/hour/minute/second/millisecond`, `weekday` (ISO 1=Mon…7=Sun), `quarter`, `day_of_year`, `iso_week` (`{week, week_year}`), `days_in_month`
- [x] 2.3 Add period-boundary ops `start_of`/`end_of` for `day|week|month|quarter|year` (week starts Monday/ISO)
- [x] 2.4 Add weekend-aware business-day ops `is_weekend`, `is_business_day`, `add_business_days`
- [x] 2.5 Add timezone support: accept an IANA `zone` on component/boundary/format/`iso` ops (resolve via `chrono-tz`), unknown zone → error; keep the canonical value a UTC instant
- [x] 2.6 Add `format(pattern, zone?)` with a locale-neutral numeric token dialect (finalize `YYYY-MM-DD` vs strftime per the design open question); keep `iso`, `unix`, `epoch_ms`
- [x] 2.7 Ensure every op is panic-free and lint-clean (checked arithmetic, no `unwrap`/`expect`/`as`), mirroring the existing date fns

## 3. JS wrapper + injection (`crates/runlet-core/src/js/`)

- [x] 3.1 Add `js/datetime.js`: the immutable `datetime` value + factory (`datetime(input)` ≡ `datetime.parse`, `datetime.now`, `datetime.parse`, `datetime.from`), all methods snake_case (`epoch_ms`), `toJSON`/`toString` → RFC 3339
- [x] 3.2 Implement the zoned *view* returned by `in_zone(zone)` — components/boundaries/format resolve in the zone, `epoch_ms()` unchanged
- [x] 3.3 Add comparison methods `cmp/eq/lt/lte/gt/gte/is_between` (no ambient-clock reads)
- [x] 3.4 Inject `datetime` as an always-on global in `engine.rs` (beside `money`/`Decimal`, unmetered); order it after the `$sys` bridge it depends on
- [x] 3.5 Remove `$sys.date` from `js/sys.js`

## 4. Determinism + type surface

- [x] 4.1 Retarget `js/determinism.js`: delete `datetime.now` (removed, not stubbed) instead of `$sys.date.now`; keep the rest of `datetime` available
- [x] 4.2 Update `engine.rs` determinism comments to name `datetime.now` (drop `$sys.date.now` references)
- [x] 4.3 Update `js/base.d.ts`: add the full `datetime` declarations (factory + value + zoned view), remove `SysDate`/`SysDateFactory`/`SysDuration`/`SysDateDiff`; regenerate `container/types.d.ts`

## 5. Tests

- [x] 5.1 Rust unit tests for the datetime domain: parsing forms, components, month/year clamping, overflow-throws, boundaries, business-day, diff/diff_in, timezone boundaries, unknown-zone error
- [x] 5.2 Determinism test: under `Profile::Deterministic`, `typeof datetime.now === "undefined"` and cannot be re-reached, while `datetime.parse`/components/arithmetic still work
- [x] 5.3 Confirm `types_dts_is_up_to_date` (D11 golden) passes; add/adjust any value-util injection test
- [x] 5.4 Add a Python harness snippet (`tests/`) exercising `datetime` end-to-end via `/execute` (immutability, ISO serialization, a timezone boundary)

## 6. Docs

- [x] 6.1 Remove the `$sys.date` section from `docs/09-sys.md` (and its mentions in `docs/README.md` / `docs/design/composable-core.md`)
- [x] 6.2 Add a beginner-friendly `datetime` capability doc under `docs/` and sync the `README.md` reference surface

## 7. Gate

- [x] 7.1 `task fmt-check`, `task clippy`, `cargo test` (via Docker) all green; `task supply-chain` passes with the new dep
