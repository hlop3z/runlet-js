## 1. JS wrapper (`crates/runlet-core/src/js/text.js`)

- [x] 1.1 Create the IIFE wrapper following `js/datetime.js` structure: an immutable `Text` value holding the plain string, `text(input)` factory that coerces to string, and `.value`/`toString`/`toJSON`/`valueOf` unwrap.
- [x] 1.2 Implement the Pythonic renames delegating to native `String.prototype`: `lower`/`upper`, `strip`/`lstrip`/`rstrip`, `starts_with`/`ends_with`, `replace`, `title`/`capitalize`/`swap_case`, `removeprefix`/`removesuffix`. Content transforms return a new `Text`.
- [x] 1.3 Implement `split`/`rsplit`/`splitlines` returning `string[]` of plain strings, and `count` returning a number.
- [x] 1.4 Implement character-class predicates `is_digit`/`is_alpha`/`is_alnum`/`is_space` returning booleans (empty-string semantics documented to match the chosen convention).
- [x] 1.5 Implement padding `zfill`/`ljust`/`rjust`/`center` over native padding, each validating requested width against the output-size cap (D4) and throwing before allocating when exceeded.
- [x] 1.6 Implement ERP verbs: `slugify` (NFD → strip combining marks → lowercase → collapse non-alnum to single `-`, no edge hyphens), `mask`/`redact` (keep-tail, default keep 4 / char `"*"`), `collapse` (trim + whitespace runs → single space), `truncate(limit, opts?)` (ellipsis marker per D-open-question resolution).

## 2. Rust injector (`crates/runlet-core/src/text.rs`)

- [x] 2.1 Create `text.rs` mirroring `datetime.rs`: `include_str!("js/text.js")` and `inject_text(qctx)` that evals the wrapper; module + fn docs noting it is pure and needs no `__sys` bridge.
- [x] 2.2 Register the module in `lib.rs` and call `inject_text` from `engine.rs` alongside the other always-on value-utils, under **both** profiles (no entry in `js/determinism.js`).

## 3. Type declarations

- [x] 3.1 Add a `Text`/`TextFactory` `.d.ts` fragment to `crates/runlet-core/src/js/base.d.ts` declaring every public method (snake_case, instance-only, no static shortcuts), prefixing interface names to keep the shared TS namespace flat.
- [x] 3.2 Regenerate `container/types.d.ts` and confirm the D11 golden test (`types_dts_is_up_to_date`) passes.

## 4. Tests

- [x] 4.1 Unit tests (Rust, in `text.rs` or a sibling test module) exercising each spec scenario through the engine: unwrap forms, immutability, renames, split/predicates, padding + oversize-refusal, and the ERP verbs (slugify diacritic fold, mask keep-tail, collapse, truncate).
- [x] 4.2 Test that `text` and all methods are present and identical under `Profile::Deterministic` (nothing removed/stubbed).
- [x] 4.3 Pin the Unicode-dependent goldens (slugify `"Café Ör 01!"` → `"cafe-or-01"`; default-locale casing) as regression guards against future engine bumps.

## 5. Docs & gate

- [x] 5.1 Add a beginner doc page under `docs/` for `text` (matching the other value-util pages), explicitly noting JS-native UTF-16 semantics, that masking is lossy display (not encryption), and the crypto/validation boundaries; update `README.md` reference.
- [x] 5.2 Run the full gate in Docker: `cargo test -p runlet-core`, `cargo clippy` (project gate, re-run until clean), and `cargo fmt --all --check`.
