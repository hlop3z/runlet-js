## Context

Date/time lives today under `$sys.date` (`js/sys.js` + the `date` domain in `sys.rs`, chrono-backed,
declared as `SysDate` in `js/base.d.ts`). It is UTC-only, minimal (`now/parse/add/sub/diff/iso/unix`),
still camelCase (`epochMs`), and namespaced under `$sys` — the system stdlib — rather than the polished
value-util tier (`$`/`money`/`Decimal`). The wall-clock reader `$sys.date.now` is *removed* (not stubbed)
under `Profile::Deterministic` by `js/determinism.js`, per the WASI "ambient authority is deleted, not
gated" lesson enforced in the `capability-registry` spec.

This change promotes date/time to a first-class `datetime` value-util and enriches it to real ERP scope:
calendar components, period boundaries, month/year arithmetic, and timezone-correct views. `runlet-core`
already depends on `chrono 0.4` (`default-features = false, features = ["clock"]`); `chrono-tz`, `icu`,
and `pure-rust-locales` are **not** in the tree today.

Full exploration (invariants, the reframe that date was not greenfield, and the tier/i18n decision) is in
this design; the proposal holds the WHY/scope.

## Goals / Non-Goals

**Goals:**
- A top-level `datetime` global mirroring the `money` factory model, snake_case, immutable, chainable.
- ERP-grade surface: components, period boundaries (`start_of`/`end_of`), calendar-aware `add`/`sub`
  (months/years with end-of-month clamping), weekend-aware business-day helpers, comparisons.
- **Timezone correctness** — the one thing UTC-only structurally cannot do ("end of month in the
  customer's timezone") — via zoned *views* over a canonical UTC instant.
- Preserve the determinism invariant exactly: `datetime.now()` removed, not stubbed, under Deterministic.
- Hard move off `$sys.date` (no dual surface); keep the D11 `types_dts_is_up_to_date` golden test green.

**Non-Goals:**
- Locale-*language* formatting (month/day names per locale) — deferred to a later increment; if ever
  added, via chrono `unstable-locales`, never ICU.
- Holiday / business-day *calendars* — country/company-specific; weekend-only here.
- Locale-format *parsing* (guessing `MM/DD` vs `DD/MM`) — a footgun; parse ISO/RFC3339/date-only/epoch
  only, with an explicit-format parse for anything else.
- Zoned *values* (Temporal `ZonedDateTime` style, zone carried everywhere) — over-built for this sandbox.
- Any change to `runlet` (HTTP front), the wire contract, or capability config/metering.

## Decisions

### Global name — `datetime`

Chosen over `dt` / `time` / `date`. It is the most standardized name for a date+time value across
Python (`datetime`), .NET/Java/Ruby (`DateTime`/`LocalDateTime`), and Luxon (`DateTime`); it matches the
sandbox's Python-like business-scripting positioning; and it accurately names the value (an instant with
both date and time). `time` is a Go-only minority and semantically reads as clock-time/duration; `date`
under-describes; `dt` is a variable-name shorthand, not a standard public API. Cost: longer than `dt`,
accepted for IntelliSense discoverability.

### Value model — canonical UTC instant + zoned *view* (A2)

The value stays a UTC instant (`epoch_ms` canonical, RFC 3339 `Z` serialization — unchanged from today).
`in_zone(tz)` returns a *view* whose components/boundaries/formatting resolve in that zone; the underlying
instant is untouched. Chosen over: **(A1)** zone-as-a-parameter on every op (verbose when many ops share a
zone) and **(A3)** zoned values carrying the zone everywhere (DST-arithmetic ambiguity + serialization
complexity — Temporal-level, over-built here). A2 = Luxon `setZone`: one zone stated once, unambiguous
canonical value.

### Determinism — `now()` removed, not stubbed

`datetime.now()` is the only ambient-clock reader; `js/determinism.js` deletes it under Deterministic
(retargeting the existing `$sys.date.now` deletion). Everything else is pure given an explicit instant, so
it stays available. Comparisons deliberately expose **no** implicit "is past/future/now" helper — those
would re-introduce the ambient clock; the author passes `datetime.now()` explicitly. This keeps the
`capability-registry` "removed, not gated" invariant intact.

### Hard move off `$sys.date` (no dual surface)

`$sys.date` is deleted, not aliased. Justified by near-zero real entanglement (no test/registered scripts
use it — only docs, the determinism hook, and engine comments reference it) and the "one canonical,
IntelliSense-discoverable form" convention, which dual-surfacing would violate. **BREAKING**, pre-1.0.

### FFI shape — reuse the `__sys` string-in/string-out JSON bridge

The date domain in `sys.rs` already routes `__sys("date", op, payloadJson)`. The enriched ops (components,
boundaries, calendar add, zoned views, format) extend the same JSON FFI — no new bridge, no rquickjs
surface growth. JS `datetime.js` holds the thin immutable wrapper; Rust holds the chrono/chrono-tz math.
(The domain may be renamed `datetime` for clarity, but the bridge mechanism is unchanged.)

### Build-vs-adopt: timezone database — **DECIDE-GATE (see `/opsx:decide`)**

Timezone views require an IANA tz database, which chrono alone does not provide. Candidate: `chrono-tz`.
This is the sole build-vs-adopt concern and is resolved in the Decisions-from-decide section below; do not
treat it as settled until `/opsx:decide` records it.

## Risks / Trade-offs

- **`chrono-tz` binary weight** → it embeds the full IANA tzdata (hundreds of KB) into the ~18 MB distroless
  image, and adds a cargo-vet subject. Mitigation: single mature dep, isolated behind the Rust date domain;
  the decide gate signs off the size/supply-chain trade explicitly; revisit a slimmer tzdb only if size bites.
- **BREAKING removal of `$sys.date`** → any out-of-repo script using it breaks. Mitigation: near-zero
  entanglement in-repo; the `sys` spec REMOVED block documents the exact migration; pre-1.0.
- **Month/year clamping is a semantic choice** (Jan 31 + 1mo → Feb 28/29) → could surprise. Mitigation:
  it is the ERP-standard behavior (matches chrono `Months`), specced explicitly with a scenario.
- **D11 golden test drift** → new `datetime` d.ts + removed `SysDate` must regenerate `container/types.d.ts`.
  Mitigation: update `base.d.ts` and the golden fixture in the same task; `types_dts_is_up_to_date` guards it.
- **Determinism regression** → forgetting to retarget the `now()` deletion would expose the wall clock under
  Deterministic. Mitigation: covered by a `datetime` spec scenario + the existing capability-registry
  invariant; add a Rust/JS test asserting `typeof datetime.now === "undefined"` under Deterministic.

## Decisions (from `/opsx:decide`)

### Decision: IANA timezone database — Adopt `chrono-tz` (full IANA)

- **Status**: approved
- **Why**: The distroless/static image has no `/usr/share/zoneinfo`, so tz data must be compiled in; `chrono-tz` (0.10.4, maintained by the chronotope org that also maintains chrono, MIT/Apache-2.0) is the chrono-native build-script embed of the full IANA db — commodity, ~100% coverage. Full (not filtered) because Tier 5's whole point is correctness for an open-ended, unknowable set of tenant/customer zones.
- **Considered**: `chrono-tz` with `filter-by-regex` (smaller image, but restricts nameable zones — trades away the correctness being adopted); hand-build / OS-tzdata (hard reject — no zoneinfo in the image, and DST-transition rules are correctness-critical, not hand-roll material); `time-tz`/`tzdb` (wrong ecosystem — target the `time` crate, not chrono).
- **Isolation**: Confined to the Rust datetime domain in `crates/runlet-core/src/sys.rs` — the JS `datetime` surface and the `__sys` string-in/string-out FFI are tz-library-agnostic; `filter-by-regex` remains a later config-only tightening if binary size is ever measured to bite.

## Migration Plan

1. Add the enriched `datetime` domain in Rust (extend/rename the `sys.rs` date domain), behind the tz
   decision from the decide gate.
2. Add `js/datetime.js` (immutable wrapper + zoned view); inject it as an always-on global in `engine.rs`
   beside `money`/`Decimal`.
3. Remove `$sys.date` from `js/sys.js`; retarget the `js/determinism.js` `now()` deletion to `datetime.now`.
4. Update `js/base.d.ts` (add `datetime` declarations, remove `SysDate`); regenerate `container/types.d.ts`.
5. Update `docs/09-sys.md` (drop date section), add a `datetime` capability doc, sync `README.md`; fix
   `engine.rs` determinism comments.
6. Rollback: revert the change set — no data/state migration, no wire/config surface touched.

## Open Questions (resolved during apply)

- **Rust FFI domain name** → **renamed to `datetime`.** The `__sys` bridge routes
  `__sys("datetime", op, payload)` (was `"date"`). Internal/cosmetic; the author-facing surface is
  `datetime` regardless.
- **`format` pattern token dialect** → **friendly `YYYY-MM-DD` numeric tokens** (`YYYY YY MM DD HH mm
  ss SSS`), not strftime. Implemented as a direct component-substitution scanner in `sys.rs`
  (`render_pattern`) rather than delegating to chrono's `format()` — chrono's `DelayedFormat` panics
  on an invalid format string at write time, which the panic-free lint gauntlet forbids; a hand
  scanner is both safe and locale-neutral by construction (any non-token char is a literal).
