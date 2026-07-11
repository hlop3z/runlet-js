## Why

Business scripts routinely need to turn a data context into a human-facing string — an invoice
body, an HTML or plain-text email, an SMS, a rendered receipt. Today an author has to hand-roll
string concatenation, which is verbose, unsafe (no HTML escaping → injection), and impossible to
express as reusable merge-tag templates. A first-class, deterministic `$std.template` closes the
last major gap in the value-util suite (`money`/`datetime`/`list`/`dict`/`text`) and is the
flagship of the remaining util roadmap.

## What Changes

- Add a new **`$std.template`** value-util backed by the **minijinja** Rust crate (Jinja2 syntax:
  `{{ }}` expressions, `{% %}` statements).
- Expose **two explicit escaping modes, no ambiguous default**:
  - `$std.template.html(source)` — HTML auto-escaping on (invoices, HTML email).
  - `$std.template.text(source)` — literal, no escaping (plain email, SMS, receipts).
  Each returns a **compiled template object** whose `.render(context)` produces the string.
- **SMB-friendly ergonomics** on the compiled template:
  - Lenient undefined variables render empty by default; `.missing("—")` sets a placeholder for
    absent merge tags.
  - `.fields()` returns the merge tags a template references (minijinja `undeclared_variables`),
    so a UI can ask "what data does this template need?".
- **Deterministic-safe by construction**: the minijinja environment is built with **no clock/random
  builtins**, so `render(source, context)` is a pure function of its inputs and `$std.template` is
  available under **both** `Profile::Full` and `Profile::Deterministic`.
- New crate dependency **minijinja** (pure Rust, no C, rides existing `serde`) → must be brought
  under cargo-vet coverage.
- The util is namespace-only (`$std.template`), consistent with `datetime`/`list`/`dict`/`text`;
  it is **not** mirrored to a bare global (only `$`/`json`/`log`/`emit` are).

## Capabilities

### New Capabilities
- `template`: Deterministic string templating over a JSON context via minijinja, with explicit
  `html`/`text` escaping modes, lenient-undefined rendering + missing-placeholder, and merge-tag
  introspection (`.fields()`).

### Modified Capabilities
- `std-namespace`: Add `$std.template` to the enumerated set of `$std` value-util members reachable
  through the namespace (it is a pure, both-profile member, so this is purely additive; it remains
  un-mirrored as a bare global).

## Impact

- **New dependency**: `minijinja` in `crates/runlet-core/Cargo.toml` (behind the same always-on
  value-util path as `money`/`datetime`); update `deny.toml`/cargo-vet exemptions or import an audit
  set so `task supply-chain` stays green.
- **Rust**: new `crates/runlet-core/src/template.rs` FFI bridge (compile + render + field
  introspection, panic-free via minijinja's `Result` API), registered in `engine.rs` alongside the
  other value-util bridges.
- **JS**: new `crates/runlet-core/src/js/template.js` wrapper defining `$std.template` on the `$std`
  object; wired into the `$std` bootstrap/freeze path.
- **Types**: new declarations in `crates/runlet-core/src/js/base.d.ts` (assembled into
  `container/types.d.ts`, guarded by the D11 golden test).
- **Docs**: a beginner guide under `docs/` and a README reference entry.
- **Tests**: Rust unit tests for the bridge (escaping, undefined handling, field extraction,
  malformed-template errors) + Python harness coverage; determinism assertion (available and pure
  under `Profile::Deterministic`).
