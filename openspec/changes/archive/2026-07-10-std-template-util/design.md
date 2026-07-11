## Context

The value-util suite (`money`, `datetime`, `list`, `dict`, `text`) is complete and consolidated
under the single `$std` namespace. `template` is the flagship of the remaining util roadmap
(`unit`, `csv`, `tax`, `pricing`, `check`). Its purpose is turning a JSON context into a
human-facing string — invoices, HTML/plain email, SMS, receipts — which business scripts currently
do with unsafe manual concatenation.

Unlike the pure-JS utils (`list`, `dict`, `text` where QuickJS's own Unicode support sufficed),
`template` is **Rust-backed**: it wraps the `minijinja` crate through an FFI bridge, the same shape
as `money`/`decimal` (backed by `rust_decimal`). It must obey the project's non-negotiables: strict
lint gauntlet (no `unwrap`/`expect`/`panic`, no bare arithmetic, no `as`), no second crypto stack
(`cargo tree -i ring` stays empty), snake_case author surface, and single-source `.d.ts` typing
guarded by the D11 golden test.

## Goals / Non-Goals

**Goals:**
- A `$std.template` util with two explicit escaping modes (`html`, `text`) and a compiled-template
  object exposing `.render(context)`, `.missing(placeholder)`, `.fields()`.
- Deterministic and identical under both `Profile::Full` and `Profile::Deterministic`.
- Panic-free: malformed templates surface as catchable JS Errors, never crash the runtime.
- SMB-friendly ergonomics: lenient undefined → empty, settable placeholder, merge-tag introspection.

**Non-Goals:**
- No custom filters/functions/tests registered by the script (keeps the environment pure and the
  attack surface small); only minijinja's built-in, deterministic filter set.
- No template *includes*/*imports* from a loader (no filesystem/registry access from a render);
  each compile takes a single inline source string.
- No clock/random/env builtins exposed to templates (would break determinism).
- No streaming/partial render, no template caching across requests (a fresh context per request is
  the model; compilation is cheap and per-invocation).
- Not the other roadmap utils (`unit`/`csv`/`tax`/`pricing`/`check`) — separate changes.

## Decisions

### Decision: Templating engine — Adopt `minijinja`

- **Status**: approved
- **Why**: Templating (escaping, control flow, expression eval) is correctness- and
  security-critical commodity functionality with a mature OSS match — squarely *adopt-over-build*.
  minijinja is pure Rust (no C, no `ring`/OpenSSL second crypto stack), has a **single required
  dependency** (`serde`, already carried), and maps 1:1 to our surface: `AutoEscape::Html`/`None` →
  `html()`/`text()`, `undeclared_variables` → `.fields()`, lenient-undefined per render. mitsuhiko's
  crates are already covered by the audit sets our cargo-vet imports pull from.
- **Considered**: `tera` (Adopt — heavier, more transitive deps, no clean per-render
  lenient-undefined toggle, no direct `.fields()` equivalent); hand-built interpolator (Build —
  rejected by the gate: escaping/control-flow is exactly what goes wrong by hand).
- **Isolation**: lives behind the `crates/runlet-core/src/template.rs` FFI bridge (the `__template`
  boundary). The JS wrapper, `$std.template.*` surface, and the `template` spec never name minijinja,
  so the engine is swappable without a contract change.
- **Guardrails**: keep the `speedups` feature **OFF** (it pulls `v_htmlescape`) — default HTML
  escaping is sufficient and keeps the footprint minimal. Verify `cargo tree -i ring` stays empty
  and bring the new dep(s) under cargo-vet coverage before `task supply-chain`.

**Detailed rationale** (kept below for the sub-decisions the gate does not cover):
Depend on `minijinja` (mitsuhiko), Jinja2 syntax, for compile + render — rides the existing `serde`
we use for context (de)serialization.

### Decision: Two explicit escaping modes, no ambiguous default
**Choice:** `$std.template.html(source)` (auto-escape on) and `$std.template.text(source)`
(no escape). No single-arg `$std.template(source)`.
**Why:** Escaping is a security decision (HTML injection in an invoice/email vs. literal SMS). A
silent default is the classic footgun — either mode is wrong half the time. Forcing the author to
name the output kind makes the safe choice explicit and self-documenting. minijinja's
`AutoEscape::Html` vs `AutoEscape::None` maps cleanly to the two entry points.
**Alternatives considered:** default-escape-with-`.raw()` opt-out (still an implicit default);
content-type sniffing (magic, non-deterministic feel). Rejected in favor of explicitness.

### Decision: Deterministic-safe environment, available under both profiles
**Choice:** Build the minijinja `Environment` with no clock/random/env globals; expose only pure
built-in filters. Register `$std.template` unconditionally (both profiles), and do **not** list it
in `__stdExpose` (namespace-only, never a bare global).
**Why:** `render(source, context)` becomes a pure function of its inputs, so it is safe under
`Profile::Deterministic` (which prunes ambient authority). Keeping it off the exposure list avoids a
surviving bare-global alias — consistent with how `datetime`/`list`/`dict`/`text` are reached only
via `$std.<name>`. This is the D2 rule from the std-namespace consolidation.
**Alternatives considered:** Full-profile-only (like `io`) — rejected: there is no I/O or
nondeterminism to gate, so restricting it would only reduce utility.

### Decision: FFI bridge shape mirrors `money`/`decimal`
**Choice:** A Rust `template.rs` module exposing a small, string-in/JSON-out FFI surface registered
into the QuickJS context in `engine.rs` next to the other value-util bridges; a `js/template.js`
wrapper defines `$std.template` and calls the bridge. Compile returns a handle the wrapper wraps in
the chainable `{ render, missing, fields }` object.
**Why:** Reuse the proven, lint-clean pattern instead of inventing a new one. minijinja's `Result`
API makes panic-free error propagation natural (map `Err` → the `__runlet`/thrown-Error contract via
the existing `__ffi.unwrap` shape used by other bridges).
**Open sub-decision (for `/opsx:apply`):** whether a compiled template is a persistent Rust-side
handle (opaque id) or re-compiled per `.render()` call. Re-compile-per-render is simpler and stays
stateless across the FFI boundary; a handle avoids recompiling for repeated `.render`. Lean
**re-compile-per-render** first (compilation is cheap, avoids cross-FFI lifetime/leak concerns and
the lint burden of holding runtime state), measure, and only add a handle cache if a hot path needs
it. The `.fields()`/`.missing()` chain is then just stored JS state (source + placeholder) applied
at render time.

## Risks / Trade-offs

- **[minijinja pulls unexpected transitive deps]** → verify with `cargo tree` before committing;
  confirm no `ring`/OpenSSL/native-tls creeps in; run `task supply-chain` and add cargo-vet
  coverage. Pin the version.
- **[Re-compile-per-render is wasteful for large templates rendered many times]** → acceptable for
  v1 (per-request model, typically one render); the handle-cache path stays open if profiling shows
  it matters. Documented as an open sub-decision, not a silent cap.
- **[minijinja's built-in filters could include something non-deterministic or ambient]** → audit
  the enabled feature set; build the `Environment` with only the default deterministic filters, no
  `Environment::new()` extras that reach the clock/env. Add a determinism test that renders the same
  input twice under `Profile::Deterministic`.
- **[Escaping-mode confusion by authors]** → mitigated by the no-default API and beginner docs that
  lead with "html for anything shown in a browser/HTML email, text for plain email/SMS".
- **[`.fields()` semantics vs nested access]** → minijinja `undeclared_variables` reports top-level
  names; document that `.fields()` returns top-level merge tags (`user`, not `user.name`).

## Migration Plan

Purely additive — a new `$std.template` member, a new spec, and a new dependency. No existing
behavior changes; the `std-namespace` enumeration gains one member. No rollback concerns beyond
dropping the dep and the two source files. Ship behind the always-on value-util path (no feature
flag), consistent with `money`/`datetime`.

## Open Questions

- Handle-cache vs recompile-per-render (see Decision above) — resolve during `/opsx:apply` with a
  quick benchmark; default to recompile.
- Should `.fields()` optionally return nested paths (`user.name`) in a later iteration? Out of scope
  for v1; top-level only.
- Do we want a `.render_or_throw` vs lenient-empty distinction, or is lenient-empty + `.missing()`
  the only mode? v1 ships lenient-only (matches SMB merge-tag expectations); a strict mode can be a
  follow-up if requested.
