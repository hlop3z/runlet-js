# Tasks — `$std.check` value-util

Model the whole thing on the `$std.template` value-util (`js/template.js` + `template.rs` +
`engine.rs` pure-util block wiring), **minus** the FFI/dependency — checksum math is pure JS, so
`check.rs` only evals the wrapper (mirror `text.rs`). Build/test are **Docker-only** (aws-lc-sys
needs a C toolchain); run `task clippy` / `cargo clippy` (not just build) before considering any
step done.

## 1. JS wrapper (`crates/runlet-core/src/js/check.js`)

- [x] 1.1 Create the IIFE wrapper following `js/template.js` structure: define `$std.check` as a
  factory that coerces its argument to a string and returns an immutable `Check` value holding it;
  the value's methods are the scheme validators. Namespace-only — do **not** add `check` to
  `__stdExpose` in `js/std.js`.
- [x] 1.2 Implement `.luhn()` (ISO/IEC 7812-1 Annex B): strip tolerated space/hyphen formatting,
  reject any remaining non-digit or empty digit-sequence with `false`, then run the right-to-left
  double/subtract-9/mod-10 pass; return a boolean.
- [x] 1.3 Implement `.gtin()` (GS1 mod-10 / ISO/IEC 15420): accept only strict digit strings of
  length 8/12/13/14 (else `false`), weight the pre-check digits alternately 3/1 from the right, and
  test that the total including the check digit is a multiple of 10; return a boolean.
- [x] 1.4 Implement `.iso7064(system)` with the piecewise modulus (`rem = (rem*10 + d) % m`, no
  BigInt): ship the `"mod_97_10"` system — map input case-insensitively (`0`–`9`→`0`–`9`,
  `A`–`Z`→`10`–`35`) then require value ≡ 1 (mod 97). Operate on the string **as given** (no IBAN
  rearrangement — the caller does that). Return `false` for an unknown `system`, out-of-alphabet
  content, or empty input; never throw. `mod_11_2`/others are deferred; keep `system` as the
  switch point.
- [x] 1.5 Ensure all three return `false` (never throw) on malformed input (D2), and that no
  `iban`/`bic`/`vat`/`isbn`/`issn` method exists on the value (D6 permanent non-goals).

## 2. Rust injector (`crates/runlet-core/src/check.rs`)

- [x] 2.1 Create `check.rs` mirroring `text.rs`: `include_str!("js/check.js")` and
  `inject_check(qctx)` that evals the wrapper; module + fn docs noting it is pure, needs no `__sys`
  bridge and no dependency.
- [x] 2.2 Add `pub mod check;` to `crates/runlet-core/src/lib.rs` and call
  `check::inject_check(&qctx)` from `engine.rs` in the pure-util block beside `text`/`collections`/
  `template`, under **both** profiles (no profile guard, no entry in `js/determinism.js`).

## 3. Type declarations

- [x] 3.1 Add `CheckFactory` + `Check` interfaces to `crates/runlet-core/src/js/base.d.ts`
  declaring `luhn()`, `gtin()`, `iso7064(system)` (snake_case, instance-only, no static shortcuts),
  each JSDoc-documented with the *consistent check digit ≠ real/registered identifier* boundary and
  the registry/publishing non-goals; wire `check: CheckFactory;` into `interface Std`.
- [x] 3.2 Regenerate `container/types.d.ts` and confirm the D11 golden test
  (`types_dts_is_up_to_date`) passes byte-equality.

## 4. Tests

- [x] 4.1 Inline `#[cfg(test)] mod tests` in `check.rs` (or `engine.rs` e2e, mirroring
  `template_tests`) driving the real JS wrapper through `engine::run`: luhn valid/invalid +
  separator tolerance, gtin EAN-13/UPC-A valid + wrong-digit + unsupported-length, iso7064
  mod_97_10 valid/corrupted + unknown-system, and malformed→`false`-not-throw for each scheme.
- [x] 4.2 Test that `$std.check` and all methods are present and identical under
  `Profile::Deterministic` (nothing removed/stubbed), and that no bare `check` global exists.
- [x] 4.3 Pin the standards goldens as regression guards: Luhn `"79927398713"`→true, EAN-13
  `"4006381333931"`→true, UPC-A `"036000291452"`→true, and the rearranged GB82 IBAN payload
  `"WEST12345698765432GB82"` `iso7064("mod_97_10")`→true (hand-verified ≡ 1 mod 97).

## 5. Docs & gate

- [x] 5.1 Add a beginner doc page under `docs/` for `check` (matching the other value-util pages),
  leading with the *consistent check digit, not real/registered* promise and listing the
  registry/jurisdiction (`iban`/`bic`/`vat`) and publishing (`isbn`/`issn`) non-goals; link it from
  `docs/README.md` and add the reference entry to the root `README.md` value-util section.
- [x] 5.2 Add a `test_check` case to `tests/test_simple.py` (registered in `main()`) exercising the
  three schemes end-to-end through `/execute`. **Note:** the full Python harness spins up its own
  box + backend sections and was not run to completion here; the identical `js/check.js` wrapper is
  covered end-to-end by the 9 in-process `check::tests` (they drive the real wrapper through
  `qctx.eval`), matching the pure-value-util precedent set by `std-template-util`.
- [x] 5.3 Ran the gate in Docker (`rust:1.92-alpine` container): `cargo test -p runlet-core`
  **123 pass** and `--no-default-features` **167 pass** (incl. all 9 `check::tests` + the D11 golden
  `types_dts_is_up_to_date`); `cargo clippy` clean under the full lint gauntlet (forced recompile of
  `runlet-core`); `cargo fmt --all --check` clean. No supply-chain step — no new dependency.
