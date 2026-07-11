# Tasks — `$std.template` value-util

Model the whole thing on the `decimal` value-util (Rust crate behind a string-in/JSON-out FFI).
Build/test are **Docker-only** (aws-lc-sys needs a C toolchain); run `task clippy` (not just build)
before considering any step done.

## 1. Dependency

- [x] 1.1 Add `minijinja` to `crates/runlet-core/Cargo.toml` as an **unconditional** `[dependencies]`
  entry beside `rust_decimal`. Done as a workspace dep (`minijinja.workspace = true`; declared in the
  root `Cargo.toml`). Kept **default features** (which pull only `serde` + `memo-map`) — the
  `speedups` feature (→ `v_htmlescape`) stays OFF, and default already gives `builtins` + render +
  `undeclared_variables`, so disabling anything would only risk a missing-feature build.
- [x] 1.2 `cargo tree -i ring` → "nothing to print" (**no** `ring`/OpenSSL/native-tls). `minijinja`
  pulls only `memo-map 0.3.3` (new) + `serde` (already in-tree).
- [x] 1.3 Brought the two new crates under cargo-vet coverage via `cargo vet add-exemption`
  (`minijinja 2.21.0`, `memo-map 0.3.3`). `cargo vet --locked` → **Vetting Succeeded**;
  `cargo deny check bans licenses sources` → **ok**. (`cargo deny check advisories` aborts on the
  pre-existing, unrelated CVSS-4.0 parse bug in the `libcrux-ecdh` RUSTSEC entry — not in our tree,
  see the [[cargo-deny-advisories-cvss4-parse-error]] memory; CI pins a working version.)

## 2. Rust FFI bridge — `crates/runlet-core/src/template.rs`

- [x] 2.1 Created `template.rs` mirroring `decimal.rs`: `include_str!("js/template.js")` +
  `inject_template(qctx)` builds a native `Function` named `__template`, sets it as a global, evals
  the wrapper.
- [x] 2.2 FFI `__template(op, source, arg2, arg3) -> String`, same envelope contract (`{"v":…}` /
  `{"list":…}` / `{"error":…}` via `sandbox::error_json`). Ops: `check` (eager syntax validation),
  `render` (context in `arg2`, `{"html","missing"}` opts in `arg3`), `fields`. `.html()`/`.text()`/
  `.missing()` are JS-side state folded into the `render` opts.
- [x] 2.3 minijinja `Environment::new()` (deterministic — no clock/random builtins);
  `AutoEscape::Html`/`None` via the auto-escape callback; `UndefinedBehavior::Chainable` for lenient
  nested access; a custom formatter emits the `.missing()` placeholder for undefined values.
- [x] 2.4 `fields` via `Template::undeclared_variables(false)` (top-level names), sorted for
  deterministic order. Documented top-level-only in the `.d.ts` + docs.
- [x] 2.5 **Panic-free**: every minijinja `Result::Err` maps to an `{"error":…}` envelope; no
  `unwrap`/`expect`/`panic`, no bare arithmetic, no `as`. Clippy-clean.
- [x] 2.6 `pub mod template;` added to `crates/runlet-core/src/lib.rs`.

## 3. JS wrapper — `crates/runlet-core/src/js/template.js`

- [x] 3.1 IIFE with `$std.template = { html, text }`, each returning a compiled-template object.
- [x] 3.2 Compiled object holds `{ source, mode, _missing }`; `.render(context)`, `.missing(placeholder)`
  (returns a new immutable object), `.fields()`. `.render` unwraps the envelope and throws
  `new Error(res.error)` on error.
- [x] 3.3 Namespace-only (`$std.template = …`); **not** added to `__stdExpose` in `js/std.js`.

## 4. Engine wiring — `crates/runlet-core/src/engine.rs`

- [x] 4.1 `use crate::template;` added beside the other util imports.
- [x] 4.2 `template::inject_template(&qctx).map_err(EngineError::internal)?;` added in the pure-util
  block (beside `collections`/`text`) — no profile guard; after bootstrap, before project/freeze.

## 5. Types — `crates/runlet-core/src/js/base.d.ts` + golden

- [x] 5.1 Added `TemplateFactory` (`html`/`text`) + `CompiledTemplate` (`render`/`missing`/`fields`)
  interfaces, JSDoc-commented.
- [x] 5.2 `template: TemplateFactory;` added to `interface Std` (no bare-global `declare const`).
- [x] 5.3 Regenerated `container/types.d.ts` (identical edit); D11 golden `types_dts_is_up_to_date`
  passes byte-equality.

## 6. Tests

- [x] 6.1 Inline `#[cfg(test)] mod tests` in `template.rs` (11 tests): html-escape vs text-verbatim,
  statements/expressions, nested access, undefined→empty, `.missing()` placeholder, `.fields()` +
  empty-for-static, malformed→error, determinism. All pass.
- [x] 6.2 Determinism + both-profile availability: covered by `engine::template_tests`
  (`available_and_pure_under_deterministic`) — drives the real JS wrapper through `engine::run` under
  both `Profile::Full` and `Profile::Deterministic`. 6 e2e tests pass.
- [x] 6.3 Python harness case `test_template` added to `tests/test_simple.py` (registered in `main()`)
  — html escaping, text verbatim, loop, lenient+placeholder, `.fields()`, malformed-throws.
  **Note:** the full harness wasn't run to completion in this Docker env (it spins up its own box +
  many backend-dependent sections); the same `/execute` end-to-end path is covered more reliably by
  the in-process `engine::template_tests` above.

## 7. Docs

- [x] 7.1 Beginner guide `docs/14-template.md` (kid-friendly), leading with html-vs-text; linked from
  `docs/README.md`.
- [x] 7.2 Reference entry added to the root `README.md` value-util section.

## 8. Gate

- [x] 8.1 `cargo clippy` clean (the project gate; lib+bins).
- [x] 8.2 `cargo fmt --all --check` clean.
- [x] 8.3 Supply-chain green: `cargo vet` succeeds, `cargo deny` bans/licenses/sources ok (advisories
  = pre-existing unrelated tooling bug, see 1.3).
- [x] 8.4 Verified the gate components individually in Docker (default `cargo test` → 114 pass;
  `--no-default-features` → 158 pass incl. 6 template e2e; clippy clean; fmt clean; supply-chain).
  **Note:** ran the components rather than one `task` invocation; the full Python harness end-to-end
  was not run to completion here (see 6.3).
- [x] 8.5 N/A — `crates/runlet-core/Cargo.toml`'s header comment lists utils illustratively
  (only `decimal`), not an exhaustive enumeration, so no edit was required.
