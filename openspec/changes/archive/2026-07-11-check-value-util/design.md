## Context

The sandbox ships value-utils for money (`$`/`money`), exact numbers (`Decimal`), dates
(`datetime`), collections (`list`/`dict`), strings (`text`), and templating (`$std.template`) —
each a chainable, immutable, snake_case surface. Checksum validation is the last common gap:
authors hand-roll Luhn / mod-10 arithmetic to sanity-check cards, barcodes, and identifiers, and
get it subtly wrong. This change adds a `$std.check` value-util in the same house style, and is
intended to be the **final** value-util — the roadmap closes after it (money-value-util, and the
project memory `value-util-stability-filter`).

The structural template is `$std.template` (`crates/runlet-core/src/js/template.js` +
`crates/runlet-core/src/template.rs`, injected from `engine.rs`'s pure-util block), **minus** the
minijinja FFI: unlike `template`, `check` needs no Rust domain crate and no `__sys` bridge because
its arithmetic is trivial. `text` is the reference for a pure-JS wrapper with a Rust injector.

Constraints that shape the design:
- The "chainable instance-method-only, snake_case, no static shortcuts" convention (CLAUDE.md;
  overrides camelCase for this business-scripting surface); `$std` members are namespace-only and
  only pure both-profile members may ever become bare globals (they are not — see D3).
- The value-util **stability filter** (project memory): only decades-stable, standards-anchored,
  registry-free, 90%-coverage utils are eligible. This design is the concrete application of that
  filter, and it hard-codes the exclusions so the surface cannot drift.
- The D11 golden test (`types_dts_is_up_to_date`) requires `base.d.ts` → `container/types.d.ts` to
  stay in sync for any surface change.
- Build/test are Docker-only (aws-lc-sys needs a C toolchain; native cargo is WDAC-blocked here).

## Goals / Non-Goals

**Goals:**
- A pure, always-on `$std.check(value)` factory injected identically under both profiles.
- Three registry-free, ISO-anchored schemes: `.luhn()` (ISO/IEC 7812-1), `.gtin()`
  (ISO/IEC 15420), `.iso7064(system)` (ISO/IEC 7064; v1 ships the `mod_97_10` system).
- A single honest promise — *consistent check digit*, never *real/registered entity* — encoded in
  return semantics (`false`, never throw, on malformed input) and stated in the `.d.ts`.
- Zero new dependency, no Rust math crate, no `__sys`/FFI bridge, no config surface, no metering.
- Full IntelliSense coverage via `base.d.ts` under `interface Std`, kept honest by the D11 test.
- A design that lets the box permanently refuse every registry/jurisdiction validator.

**Non-Goals:**
- Registry- or jurisdiction-dependent validation: branded `iban`/`bic`/`vat` format checks and
  national-ID format tables. These depend on living data (SWIFT IBAN registry, per-country VAT
  rules) that rots — the exact thing the stability filter excludes.
- Publishing-only schemes `isbn` (ISO 2108) / `issn` (ISO 3297): ISO-stable but fail the 90% bar.
- Generating or repairing check digits (compute-the-missing-digit): validation only for v1.
- Detecting *which* scheme a value belongs to (auto-dispatch across luhn/gtin/iso7064): the author
  names the scheme; guessing would be lossy and un-Pythonic.

## Decisions

### D1 — Pure-JS wrapper, no Rust domain math, no new dependency

**Decision:** Implement all three schemes as a pure-JS wrapper (`js/check.js`) over plain `Number`
arithmetic, with only a minimal Rust injector (`check.rs::inject_check`) that evals the wrapper.
No crate, no `__sys`/FFI bridge, no BigInt.

**Why:** Luhn and GS1 mod-10 are single passes of add/double/mod-10 over ≤14 digits. The one
operation that looks like it needs big-number math — ISO 7064 MOD 97-10 over a ~30-char rearranged
IBAN — is computed with the standard **piecewise modulus** (fold left-to-right: `rem = (rem*10 +
digit) % 97`), where every intermediate stays well under 2^53, so plain `Number` is exact. Nothing
needs a dependency; adopting one would *add* a supply-chain review + an FFI round-trip for zero
correctness gain. Mirrors the `text` build-vs-adopt call.

**Alternatives considered:**
- **Rust `__sys("check")` bridge** (e.g. a `luhn`/`iban` crate): total control but a dep +
  cargo-vet review + serialization per call, for arithmetic we can write in ~40 lines of JS.
  Rejected.
- **BigInt for MOD 97-10:** unnecessary given the piecewise modulus; also keeps the code portable
  across QuickJS BigInt support. Rejected.

### D2 — `false`, never throw, on malformed input

**Decision:** Every scheme method returns a boolean; wrong length, out-of-alphabet characters,
empty input, and (for `iso7064`) an unknown `system` all return `false` rather than throwing.

**Why:** A validator is called on untrusted input in the hot path (`if ($std.check(x).luhn())`).
Throwing on malformed input would force authors to wrap every call in try/catch and would conflate
"badly formed" with "failed the check" — both mean *not valid*, which is `false`. This matches how
the audience reasons about a yes/no check. (Contrast `text`'s padding, which throws on an
oversize-allocation *programming* error — there is no such unbounded-allocation risk here.)

### D3 — Namespace-only member, not a bare global

**Decision:** `js/check.js` defines `$std.check = <factory>` and does **not** add `check` to
`__stdExpose` in `js/std.js`. Reached only as `$std.check`.

**Why:** Matches `$std.template`. The bare-global projection is reserved for the few oldest,
highest-traffic utils (`$`, `json`, `log`, `emit`); new utils live under the namespace to keep the
global surface small and avoid shadowing common author locals like `check`.

### D4 — Factory-instance shape, wrap-once then pick scheme

**Decision:** `$std.check(value)` coerces the argument to a string and returns an immutable value
whose methods are the schemes (`luhn`/`gtin`/`iso7064`). No static shortcuts on the factory
(no `$std.check.luhn(x)`).

**Why:** The convention is chainable-instance-methods-only, no static shortcuts. The value is not a
transform chain (each method is terminal, returning a boolean), but the wrap-then-scheme shape is
consistent with the other value-utils and reads naturally: `$std.check(card).luhn()`. Normalization
that is common across schemes (stripping tolerated space/hyphen formatting for Luhn) happens per
scheme, not in the factory, so each scheme controls its own accepted alphabet.

### D5 — Injected under both profiles; no determinism sanitizer entry

**Decision:** `engine.rs` injects `check` in the pure-util block beside `text`/`collections`/
`template`, under both `Full` and `Deterministic`. Nothing is added to `js/determinism.js`.

**Why:** Checksum math touches no clock, randomness, or ambient authority. There is nothing to
prune — the same reasoning that makes `text`/`template` unconditional.

### D6 — `iso7064` as the escape hatch that closes the roadmap

**Decision:** Expose the generic `iso7064(system)` primitive (v1: the `mod_97_10` system; `system`
is the extension point) but ship **no** branded validator built on it, and document
`iban`/`bic`/`vat`/`isbn`/`issn` as permanent non-goals in the spec, the `.d.ts`, and the beginner
doc. The primitive operates on the string as given — the caller rearranges an IBAN before calling
(it holds no IBAN-structure or rearrangement logic of its own), keeping it a truly generic ISO 7064
check, not a jurisdictional one.

**Why:** IBAN/LEI/national-ID checksums are *instances* of ISO 7064; exposing the raw standard lets
a script validate the checksum arithmetic itself without the box ever owning the SWIFT country
registry or per-jurisdiction rule tables (the data that rots). This is what makes it safe to
declare the value-util roadmap closed: any future "please add IBAN/VAT" is answered by the
existing primitive plus the documented non-goal, not a new dependency-bearing validator.
`mod_11_2` and other ISO 7064 systems are deferred (not shipped unverified) — the `system`
argument admits them later without an API change.

## Build-vs-Adopt Decisions

### Decision: Checksum algorithm correctness (Luhn / GS1 mod-10 / ISO 7064) — Build (hand-written pure-JS)

- **Status**: approved (user-confirmed 2026-07-11)
- **Why**: The algorithms are public-domain, ISO-specified error-detection arithmetic (not
  security primitives), and tiny — single-pass add/double/mod-10; MOD 97-10 via piecewise modulus,
  no BigInt. Correctness is fully guarded by pinned ISO golden vectors (tasks 4.3). Adopting a
  crate would *add* a `__sys` FFI bridge + serialization + a cargo-vet review + an **unconditional
  `runlet-core` dependency even in the `--no-default-features` core** (`check` is always-on), to
  replace ~40 lines of JS — more build surface, not less. Mirrors the `text` value-util decision.
- **Considered**: **Adopt `codes-check-digits`** (one crate covering Luhn + GS1 + ISO 7064 — the
  strongest adopt candidate, but drags in the FFI bridge + cargo-vet + a permanent core dep);
  **Adopt `iso_iec_7064`** (clean, conforming, but covers only 1 of 3 schemes); **Adopt
  `iban_validate`** (rejected on scope — it is the SWIFT-registry branded IBAN validator this
  change defines as a permanent non-goal, per D6); BigInt for MOD 97-10 (unneeded given the
  piecewise fold).
- **Isolation**: entirely inside `js/check.js`; swappable to a Rust `__sys("check")` bridge later
  without touching `specs/` or `config`.

## Risks / Trade-offs

- **[Authors read "valid" as "real/registered"]** → A checksum-valid card or IBAN can still be a
  nonexistent account. Mitigation: the spec's consistent-check-digit-vs-existence requirement, the
  `.d.ts` doc wording, and the beginner doc all state the boundary explicitly.
- **[Pressure to add IBAN/VAT/national-ID validators]** → Mitigation: D6 — the `iso7064` primitive
  plus the documented permanent non-goal absorb the request without taking on registry data.
- **[MOD 97-10 precision]** → If implemented as one big multiplication it would exceed 2^53.
  Mitigation: the piecewise modulus keeps every step small; a golden test pins the known-valid IBAN
  payload as a regression guard.
- **[Separator-tolerance ambiguity]** → Tolerating spaces/hyphens in `luhn` but not elsewhere could
  surprise. Mitigation: the accepted alphabet is stated per scheme in the spec and `.d.ts`; GTIN
  and iso7064 accept only their strict alphabets.
- **[QuickJS behavior drift across engine bumps]** → Low: only basic `Number` arithmetic and string
  iteration are used. Mitigation: the golden scenario values become unit tests run in CI.

## Migration Plan

Additive, no breaking changes. Deploy adds one `$std` namespace member; no data or config
migration. Rollback is removing the `check::inject_check` call in `engine.rs` (and regenerating
`types.d.ts`). A script's own `check` local is unaffected (the util is namespace-only).

## Open Questions

- **ISO 7064 system coverage for v1** — RESOLVED: ship `mod_97_10` only (the IBAN/LEI check, and
  the one with a hand-verified conformance golden). `mod_11_2` and the alphabetic/double systems
  (`mod_37_2`, `mod_37_36`, `mod_11_10`) are deferred rather than shipped without a trustworthy
  vector; the `system` argument is the extension point.
- **Luhn separator tolerance** — ignore only spaces/hyphens, or any non-alphanumeric? Lean:
  spaces and hyphens only (the formatting authors actually paste), everything else → `false`.
- **GTIN length set** — include GTIN-14 (logistics) or restrict to the retail 8/12/13? Lean:
  include 8/12/13/14 since they share one algorithm and the length-dispatch is free.
