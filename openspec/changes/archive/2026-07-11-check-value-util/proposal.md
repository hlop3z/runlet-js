## Why

The sandbox is a business-scripting language and ships first-class value-utils for money
(`$`/`money`), exact numbers (`Decimal`), dates (`datetime`), collections (`list`/`dict`),
strings (`text`), and templating (`$std.template`). The last common business-logic gap is
**checksum validation**: authors constantly need to confirm that a credit-card number, a retail
barcode, or an identifier's check digit is internally consistent, and today they hand-roll the
Luhn or mod-10 arithmetic each time (easy to get subtly wrong) or skip the check entirely. This
is exactly the logic that gets duplicated in a frontend for UX but *must* be re-verified
server-side because the client is never trusted — so it belongs here as the authoritative copy.

A `$std.check` value-util closes that gap with a small, deterministic, standards-anchored surface
— and, deliberately, it is the **last** value-util: the algorithms it covers are ISO-defined and
decades-stable, and its design draws a hard line that lets the box refuse every churning,
registry-dependent validator (IBAN-format, BIC, VAT) forever.

## What Changes

- **New `$std.check(value)` value-util**, a namespace member beside `$std.template`. `$std.check`
  is a factory that wraps a value; its methods each answer one question — *"is this string's
  check digit internally consistent?"* — and return a `boolean`. It is **not** a chain of
  transforms; the wrapped value is validated by the scheme method the author picks.
- **Three checksum schemes, each ISO/standards-anchored and registry-free:**
  - `.luhn()` — the Luhn mod-10 algorithm (ISO/IEC 7812-1 Annex B): credit/debit cards, IMEI.
  - `.gtin()` — the GS1 mod-10 check digit (ISO/IEC 15420): UPC-A (GTIN-12), EAN-13 (GTIN-13),
    GTIN-8 and GTIN-14. Length-dispatched over the accepted GTIN widths.
  - `.iso7064(system)` — the raw ISO/IEC 7064 check-character standard (v1 ships the `mod_97_10`
    system; `system` is the documented extension point for more). This is the deliberate **escape
    hatch**: a script that needs an IBAN's mod-97 check digit rearranges the IBAN and computes it
    here itself, so the box never ships — and never has to maintain — a branded jurisdictional
    validator.
- **One honest, non-negotiable promise, stated in the `.d.ts` and docs:** a `check` method
  verifies that the *check digit is consistent*, never that the entity is *real or registered*. A
  checksum-valid card/IBAN can still correspond to no account. This boundary is what keeps the
  util pure and standards-only.
- **Explicit, permanent non-goals** (documented so the surface never drifts): no registry- or
  jurisdiction-dependent validation — branded `iban`/`bic`/`vat` format checks and national-ID
  format tables are refused because they depend on living data that rots; and no publishing-only
  schemes (`isbn`/`issn`) because, though ISO-stable, they fail the "covers the 90%" bar.
- **Zero new dependency, no Rust domain math.** Luhn, GS1 mod-10, and the ISO 7064 piecewise
  mod-97 all reduce to small integer arithmetic that plain-JS `Number` handles exactly (no
  BigInt, no crate). Structurally this mirrors the `text`/`template` pure-util pattern: a JS
  wrapper (`js/check.js`) plus a ~20-line Rust injector (`check.rs::inject_check`) that evals it,
  with **no** `__sys`/FFI bridge.
- **Injected identically under both `Profile::Full` and `Profile::Deterministic`** — checksum math
  touches no clock, no randomness, no ambient state, so there is nothing for the determinism
  sanitizer to remove and no entry in `js/determinism.js`.
- **Namespace-only.** `$std.check` is reached through the `$std` namespace and is **not** added to
  `__stdExpose` in `js/std.js` (no bare global), matching `$std.template`.
- **`check.d.ts` fragment** added to `base.d.ts` under `interface Std` so every method is
  IntelliSense-discoverable; the D11 golden test (`types_dts_is_up_to_date`) keeps
  `container/types.d.ts` in sync.

## Capabilities

### New Capabilities
- `check`: the `$std.check` checksum-verification value-util — a factory wrapping a value with
  scheme methods (`luhn`, `gtin`, `iso7064`) that each return a boolean asserting an internally
  consistent check digit (never registration/existence). Pure, deterministic, always-on under
  both profiles, standards-anchored (ISO/IEC 7812-1, 15420, 7064), with registry/jurisdiction and
  publishing-only schemes as explicit permanent non-goals.

### Modified Capabilities
<!-- None. `$std.check` is a net-new namespace member; no existing spec's requirements change. -->

## Impact

- **New code:** `crates/runlet-core/src/js/check.js` (the wrapper), `crates/runlet-core/src/check.rs`
  (the `inject_check` injector), a `Check`/`CheckFactory` `.d.ts` fragment folded into
  `crates/runlet-core/src/js/base.d.ts` under `interface Std`.
- **Wiring:** `pub mod check;` in `lib.rs`; `check::inject_check(&qctx)` called in `engine.rs`'s
  pure-util block (beside `text`/`collections`/`template`, no profile guard); regenerate
  `container/types.d.ts` so the D11 golden test passes.
- **No new dependency, no Rust math crate, no `__sys`/FFI bridge, no config surface, no metering**
  (checksum ops do not count toward `max_ops` and produce no `meta` metrics).
- **Docs:** a beginner guide page under `docs/` (matching the other value-util pages), stating the
  consistent-check-digit-vs-real-identifier boundary and the registry/publishing non-goals;
  README reference entry; a `test_check` case in `tests/test_simple.py`.
- **No breaking changes.** Adds one `$std` namespace member; nothing existing changes.
