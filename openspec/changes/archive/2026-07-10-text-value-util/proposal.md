## Why

The sandbox is a business-scripting language and already ships first-class value-utils for
money (`$`/`money`), exact numbers (`Decimal`), and dates (`datetime`) — but string handling
is left to raw JS, whose method surface is camelCased, verbose (`toLowerCase`, `padStart`,
`trimStart`), and missing the shaping verbs business/ERP scripts reach for constantly
(slugify, mask a card/account tail, collapse whitespace, zero-pad a reference code). Authors
either hand-roll these each time (error-prone, inconsistent) or import nothing and produce
inconsistent output. A `text` value-util closes the last obvious gap in the value-util set with
a uniform, snake_case, IntelliSense-discoverable surface.

## What Changes

- **New always-on `text` global**, a chainable immutable value-util beside `$`/`money`/
  `Decimal`/`datetime`. `text("...")` wraps a string; methods return new `text` values;
  `.value`/`toString`/`toJSON` unwrap back to a plain string.
- **Pythonic-named core** — snake_case renames that delegate verbatim to native
  `String.prototype`: `lower`/`upper`/`strip`/`lstrip`/`rstrip`, `starts_with`/`ends_with`,
  `replace`, `split`/`rsplit`/`splitlines`, `count`, `zfill`/`ljust`/`rjust`/`center`,
  `title`/`capitalize`/`swap_case`, `removeprefix`/`removesuffix`,
  `is_digit`/`is_alpha`/`is_alnum`/`is_space`. Semantics (including UTF-16 code-unit
  counting/width) are JS's — we rename, we do not reimplement.
- **Small ERP verb set** composed from those same JS primitives: `slugify` (NFD-fold
  diacritics → ASCII kebab), `mask`/`redact` (keep-tail), `collapse` (whitespace),
  `truncate` (with ellipsis), and code/reference normalization+padding.
- **Zero new dependency, no Rust domain math.** A spike confirmed `String.prototype.normalize`
  is available in this QuickJS build and casing is Unicode-default/locale-independent, so the
  whole surface — slugify included — is a pure-JS wrapper. Structurally this mirrors the
  `datetime` value-util (thin JS wrapper + a ~30-line Rust injector), minus the `__sys` bridge.
- **Injected identically under both `Profile::Full` and `Profile::Deterministic`** — text ops
  touch no clock, no randomness, no ambient state, so there is nothing for the determinism
  sanitizer to remove.
- **Output-size guard** — width/repeat inputs (`zfill`/`ljust`/`rjust`/`center`) are
  caller-controlled and could OOM; the util caps produced length in the spirit of the engine's
  `max_*_bytes` limits.
- **Boundaries held explicitly.** `text` does human-readable *shaping* (including lossy
  masking) and is distinct from `$sys.crypto`/codec (reversible byte encoding, hmac, uuid);
  character-class predicates (`is_digit`) live here while semantic-domain predicates
  (`is_email`) are reserved for a future validation util.
- **`text.d.ts` fragment** added to `base.d.ts` so every method is IntelliSense-discoverable;
  the D11 golden test (`types_dts_is_up_to_date`) keeps `container/types.d.ts` in sync.

## Capabilities

### New Capabilities
- `text`: the chainable, immutable, snake_case string value-util — Pythonic-named passthroughs
  over native JS string operations plus a small ERP shaping verb set (slugify/mask/collapse/
  truncate/pad), always injected under both profiles, pure and deterministic, with a
  caller-controlled output-size guard.

### Modified Capabilities
<!-- None. `text` is a net-new always-on global; no existing spec's requirements change. -->

## Impact

- **New code:** `crates/runlet-core/src/js/text.js` (the wrapper), `crates/runlet-core/src/text.rs`
  (the injector), a `text.d.ts` fragment folded into `crates/runlet-core/src/js/base.d.ts`.
- **Wiring:** `engine.rs` injects `text` alongside the other always-on value-utils; regenerate
  `container/types.d.ts` so the D11 golden test passes.
- **No new dependency, no Rust math crate, no `__sys`/FFI bridge, no config surface, no metering.**
- **No breaking changes.** Adds a global; a script's own `text` local shadows it harmlessly
  (the reason the factory is named `text`, not `str`).
