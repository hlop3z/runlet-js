/**
 * Type definitions for the **jsbox** sandbox.
 *
 * These describe the globals available inside the `handler(ctx)` function you
 * `POST /execute`. Keep this file beside your script and the shipped
 * `tsconfig.json` for editor autocomplete and type-checking out of the box.
 *
 * The bundled `tsconfig.json` already enables `checkJs`, so any top-level `.js`
 * script in this folder is checked automatically — just write your handler:
 * ```js
 * /** @type {Handler} *\/
 * function handler(ctx) {
 *   return json({ hi: ctx.name }, null);
 * }
 * ```
 * (No `/// <reference>` or `// @ts-check` needed; the tsconfig wires it up.)
 *
 * @remarks
 * **Capabilities are opt-in.** The box ships three built-ins. `io` (logical
 * egress) exists when the request lists a resource name in `config.io` — a
 * **flat allowlist** of logical names, e.g. `"io": ["orders", "cache"]`; call a
 * named resource with `io.call(name, action, payload)`. Each name is an
 * operator-defined resource resolved box-direct or by a broker — the request
 * never carries endpoints or credentials. The in-engine capabilities keep their
 * own config: `api` exists when `config.allowed_hosts` is non-empty, `s3` when
 * `config.s3` is present. Otherwise the global is `undefined` (e.g.
 * `typeof s3 === "undefined"`). They are declared here as
 * always-present for convenient autocomplete; guard with `typeof` if a
 * capability is optional.
 * `json`, `$`, `Decimal`, `datetime`, and `$sys.crypto` are pure and **always**
 * available; `$sys.env` / `$sys.secrets` populate only when `config.sys` is set.
 *
 * `eval` and `Proxy` are removed before your `handler` runs.
 */

// ─────────────────────────────────────────────────────────────────────────────
// Response envelope
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Builds the `{ data, error }` envelope your `handler` must return. The server
 * attaches `meta` and replies with `{ data, error, meta }`.
 *
 * Pass `null` for whichever side doesn't apply.
 *
 * @param data  The success payload (any JSON-serializable value, or `null`).
 * @param error The error payload (any JSON-serializable value, or `null`).
 *
 * @example
 * return json({ ok: true }, null);     // success
 * @example
 * return json(null, { message: "bad input" }); // failure
 */
declare function json(data: unknown, error?: unknown): string;

/**
 * Proposes a **tagged effect** — a structured thing your handler wants the platform to record or
 * act on, separate from its `return` value. Effects are surfaced on the response as an ordered
 * `effects: [{ kind, value }]` list, captured **even if your handler later throws** (so a
 * partial run keeps everything it emitted). "Logic proposes, the host disposes."
 *
 * @param kind  A required non-empty routing tag (e.g. `"decided"`, `"finding"`, `"email"`),
 *              at most 64 characters. The platform routes/governs by `kind`; it never
 *              interprets what it means.
 * @param value Any JSON-serializable value — opaque to the platform, passed through verbatim.
 *
 * @example
 * emit("decided", { tier: "tier-3", reason: "spend > 10k" }); // an audit/decision trail
 * @example
 * for (const m of mismatches) emit("finding", m); // itemized findings, kept even on a later throw
 */
declare function emit(kind: string, value: unknown): void;

/**
 * A structured, leveled diagnostic logger — **diagnostics, not billing; lossy by design**. Unlike
 * {@link emit} (platform-facing effects the host always keeps), `log.*` is developer-facing:
 * level-filtered, capped per execution, and dropped under backpressure. Deliberately **not**
 * `console.log` — a stateless box behind a gateway cannot promise "prints to a stream you own".
 *
 * Each call takes a **Serilog-style message template** with `{name}` placeholders plus a properties
 * object; the entry keeps the template, the named properties, and the rendered message. Where the
 * entry goes (a tenant stream, and/or inline on the response) is **platform policy** — the script
 * never chooses a sink. Entries logged before a handler throws are still captured.
 *
 * @example
 * log.info("charged {user} {amount}", { user: 42, amount: "10.00" }); // → "charged 42 10.00"
 * @example
 * const l = log.with({ requestId }); // bound context on every entry from `l`
 * l.warn("retrying {attempt}", { attempt });
 */
interface Logger {
  /** Finest-grained tracing (below `debug`). */
  trace(template: string, properties?: Record<string, unknown>): void;
  /** Debugging detail (off in production by default). */
  debug(template: string, properties?: Record<string, unknown>): void;
  /** Normal operational events (the default level floor). */
  info(template: string, properties?: Record<string, unknown>): void;
  /** A warning that did not stop the run. */
  warn(template: string, properties?: Record<string, unknown>): void;
  /** An error condition. */
  error(template: string, properties?: Record<string, unknown>): void;
  /**
   * Derives a logger with **bound context** merged into every subsequent entry's properties
   * (Pino-style child). A per-call property overrides a bound key of the same name.
   */
  with(fields: Record<string, unknown>): Logger;
}

/**
 * The diagnostic logger (always available). Below-floor levels are discarded cheaply; captured
 * entries are lossy and routed by platform policy. See {@link Logger}.
 */
declare const log: Logger;

/**
 * The function the sandbox calls. Define `function handler(ctx) { ... }` in your
 * script; it receives the request's `context` and must return {@link json}`(...)`.
 *
 * @example
 * /** @type {Handler} *\/
 * function handler(ctx) {
 *   return json({ hello: ctx.name }, null);
 * }
 */
type Handler = (ctx: any) => string;

// ─────────────────────────────────────────────────────────────────────────────
// `Decimal` — exact number math · `$` / `money` — currency-safe money (always available)
// ─────────────────────────────────────────────────────────────────────────────

/** A value accepted anywhere an exact number is expected. */
type DecimalInput = number | string | Decimal;

/**
 * The rounding strategy for `Decimal.round`/`round_to` and `Money.round`. Adopts the
 * standard Java `RoundingMode` meaning, spelled snake_case:
 * - `"half_up"` — round half away from zero (the default; commercial rounding).
 * - `"half_even"` — banker's rounding (ties to the even neighbour), for ledgers.
 * - `"up"` / `"down"` — always away from / toward zero.
 * - `"ceil"` / `"floor"` — toward +∞ / −∞.
 */
type RoundingMode = "half_up" | "half_even" | "up" | "down" | "ceil" | "floor";

/**
 * An exact, arbitrary-precision decimal for **non-money** numbers (quantities, rates,
 * weights, percentages, ratios). JavaScript has no operator overloading, so arithmetic
 * is method-based and **immutable** — every operation returns a new `Decimal`. Backed by
 * the same engine that decodes Postgres `NUMERIC`, so it round-trips DB decimals without
 * precision loss. For currency use {@link Money} (`$`), which is currency-safe.
 *
 * @example
 * const total = Decimal("0.1").add("0.2");   // exact 0.3, not 0.30000000000000004
 * total.to_string();                         // "0.3"
 */
interface Decimal {
  /** Returns `this + other`. */
  add(other: DecimalInput): Decimal;
  /** Returns `this - other`. */
  sub(other: DecimalInput): Decimal;
  /** Returns `this * other`. */
  mul(other: DecimalInput): Decimal;
  /** Returns `this / other`. */
  div(other: DecimalInput): Decimal;
  /** Returns `-this`. */
  neg(): Decimal;
  /** Returns `|this|`. */
  abs(): Decimal;
  /** Rounds to `places` decimal places (default `0`) using `mode` (default `"half_up"`). */
  round(places?: number, mode?: RoundingMode): Decimal;
  /**
   * Rounds to the nearest multiple of `step` using `mode` (default `"half_up"`).
   * @example Decimal("2.03").round_to("0.05");   // 2.05  (cash rounding)
   */
  round_to(step: DecimalInput, mode?: RoundingMode): Decimal;
  /** `p` percent of the value: `this * p / 100`. */
  pct(p: DecimalInput): Decimal;
  /** The value constrained to the inclusive `[lo, hi]` range. */
  clamp(lo: DecimalInput, hi: DecimalInput): Decimal;
  /** The smaller of `this` and `other`. */
  min(other: DecimalInput): Decimal;
  /** The larger of `this` and `other`. */
  max(other: DecimalInput): Decimal;
  /** Compares: returns `-1` if `this < other`, `0` if equal, `1` if greater. */
  cmp(other: DecimalInput): number;
  /** `this === other`. */
  eq(other: DecimalInput): boolean;
  /** `this < other`. */
  lt(other: DecimalInput): boolean;
  /** `this <= other`. */
  lte(other: DecimalInput): boolean;
  /** `this > other`. */
  gt(other: DecimalInput): boolean;
  /** `this >= other`. */
  gte(other: DecimalInput): boolean;
  /** `true` if the value is exactly zero. */
  is_zero(): boolean;
  /** `true` if the value is less than zero. */
  is_negative(): boolean;
  /** `true` if the value is greater than zero. */
  is_positive(): boolean;
  /** The exact value as a decimal string (e.g. `"19.99"`). */
  toString(): string;
  /** The value as a JS `number` — may lose precision for large/long decimals. */
  to_number(): number;
  /** Serializes as the exact string value inside {@link json} / `JSON.stringify`. */
  toJSON(): string;
  /** @deprecated Use {@link is_zero}. */
  isZero(): boolean;
  /** @deprecated Use {@link is_negative}. */
  isNegative(): boolean;
  /** @deprecated Use {@link to_number}. */
  toNumber(): number;
}

/** Creates a {@link Decimal} from a number, string, or another `Decimal`. */
interface DecimalFactory {
  (value?: DecimalInput): Decimal;
}

/** Exact-number factory (non-money). Always available. Distinct from `$` (money). */
declare const Decimal: DecimalFactory;

/** An ISO 4217 currency code (e.g. `"USD"`, `"EUR"`, `"JPY"`). */
type CurrencyCode = string;

/** The self-describing shape a {@link Money} serializes to in {@link json} / `JSON.stringify`. */
interface MoneyJSON {
  /** The exact amount as a decimal string (e.g. `"19.99"`). */
  amount: string;
  /** The ISO 4217 currency code. */
  currency: CurrencyCode;
  /** Integer minor units, currency-correct (USD `1999`, JPY `1000`, BHD `1234`). */
  minor_units: number;
}

/**
 * A currency-bound money value — **safe by construction**. Arithmetic only combines the
 * **same** currency (no implicit FX), precision follows the currency's ISO 4217 minor unit
 * (USD 2 places, JPY 0, BHD 3), and splitting is penny-safe. Immutable: every op returns a
 * new `Money`. Use {@link Decimal} for non-money numbers.
 *
 * @example
 * const total = $("100.00", "USD").add_pct(8.25);   // 108.25 USD (tax)
 * total.allocate_to(3);                              // [36.09, 36.08, 36.08], sums exactly
 */
interface Money {
  /** Returns `this + other` (same currency; else throws). */
  add(other: Money): Money;
  /** Returns `this - other` (same currency; else throws). */
  sub(other: Money): Money;
  /** Returns `this * scalar`. Multiplying money by money throws. */
  mul(scalar: DecimalInput): Money;
  /**
   * Divides by a scalar → `Money`, or by same-currency money → a dimensionless
   * {@link Decimal} ratio (margin/variance/growth). Cross-currency division throws.
   */
  div(other: DecimalInput | Money): Money | Decimal;
  /** Returns `-this`. */
  neg(): Money;
  /** Returns `|this|`. */
  abs(): Money;
  /** `p` percent of the amount, rounded to the currency precision (e.g. tax). */
  pct(p: DecimalInput): Money;
  /** The amount increased by `p` percent, rounded to the currency precision (tax/markup). */
  add_pct(p: DecimalInput): Money;
  /** The amount decreased by `p` percent, rounded to the currency precision (discount). */
  sub_pct(p: DecimalInput): Money;
  /** Rounds to the currency's minor unit using `mode` (default `"half_up"`). */
  round(mode?: RoundingMode): Money;
  /**
   * Splits the amount by `weights` so the shares sum to the total **exactly** (largest-remainder /
   * Hamilton method; leftover minor units go to the largest remainders, ties by input order).
   * @example $("0.05", "USD").allocate([70, 30]);   // [0.04, 0.01]
   */
  allocate(weights: number[]): Money[];
  /** Equal penny-safe split into `n` shares (sums to the total exactly). */
  allocate_to(n: number): Money[];
  /** Alias of {@link allocate_to}. */
  split(n: number): Money[];
  /** Compares same-currency amounts: `-1` / `0` / `1` (cross-currency throws). */
  cmp(other: Money): number;
  /** `this === other` (same currency). */
  eq(other: Money): boolean;
  /** `this < other` (same currency). */
  lt(other: Money): boolean;
  /** `this <= other` (same currency). */
  lte(other: Money): boolean;
  /** `this > other` (same currency). */
  gt(other: Money): boolean;
  /** `this >= other` (same currency). */
  gte(other: Money): boolean;
  /** `true` if the amount is exactly zero. */
  is_zero(): boolean;
  /** `true` if the amount is negative. */
  is_negative(): boolean;
  /** `true` if the amount is positive. */
  is_positive(): boolean;
  /** Integer minor units for a payment API (USD `1999`, JPY `1000` — zero-decimal-correct). */
  to_minor(): number;
  /** The amount as a currency-less {@link Decimal}. */
  amount(): Decimal;
  /** The ISO 4217 currency code. */
  currency(): CurrencyCode;
  /** A human display string (e.g. `"$19.99"`). */
  format(): string;
  /** The exact amount string, currency omitted (e.g. `"19.99"`). */
  to_string(): string;
  /** Same as {@link to_string} — the amount only. */
  toString(): string;
  /** The amount as a lossy JS `number`. */
  to_number(): number;
  /** Serializes as {@link MoneyJSON} inside {@link json} / `JSON.stringify`. */
  toJSON(): MoneyJSON;
}

/**
 * Creates a {@link Money} value. `$` and `money` are the same constructor. The currency
 * resolves through a cascade: the explicit `currency` argument, else the per-request
 * `config.currency`, else the operator `default_currency`; if none is set, construction throws.
 *
 * @example
 * const price = $("19.99", "USD");   // explicit currency
 * const tax = $("19.99").pct(8.25);  // currency from config.currency / default
 */
interface MoneyFactory {
  (amount: DecimalInput | Money, currency?: CurrencyCode): Money;
}

/** Money factory (currency-bound). Always available. Same as {@link money}. */
declare const $: MoneyFactory;
/** Money factory (currency-bound). Always available. Same as {@link $}. */
declare const money: MoneyFactory;

// ─────────────────────────────────────────────────────────────────────────────
// `datetime` — immutable UTC instant + timezone-aware views (always available)
// ─────────────────────────────────────────────────────────────────────────────

/** A value accepted anywhere a {@link DateTime} is expected: an ISO/RFC-3339 string, a
 * `YYYY-MM-DD` string, epoch milliseconds, or an existing {@link DateTime}. */
type DateTimeInput = string | number | DateTime;

/** The period unit for {@link DateTime.start_of} / {@link DateTime.end_of}. Weeks start Monday (ISO). */
type DateTimeUnit = "day" | "week" | "month" | "quarter" | "year";

/** The whole-unit for {@link DateTime.diff_in}. */
type DateTimeDiffUnit = "ms" | "seconds" | "minutes" | "hours" | "days" | "weeks";

/** Calendar/clock parts for {@link DateTimeFactory.from}. `month` is 1–12, `day` 1–31. */
interface DateTimeParts {
  year: number;
  month: number;
  day: number;
  hour?: number;
  minute?: number;
  second?: number;
  millisecond?: number;
}

/**
 * A shift for {@link DateTime.add} / {@link DateTime.sub}. `years`/`months` are calendar units
 * (end-of-month-clamped: Jan 31 + 1 month → Feb 28/29); the rest are fixed-length.
 */
interface DateTimeDelta {
  years?: number;
  months?: number;
  weeks?: number;
  days?: number;
  hours?: number;
  minutes?: number;
  seconds?: number;
  ms?: number;
}

/** The gap between two instants, from {@link DateTime.diff}. */
interface DateTimeDiff {
  /** Signed total milliseconds (`this - other`). */
  total_ms: number;
  /** Signed total seconds. */
  total_seconds: number;
  /** Whole days in the absolute gap. */
  days: number;
  /** Remaining whole hours (0–23). */
  hours: number;
  /** Remaining whole minutes (0–59). */
  minutes: number;
  /** Remaining whole seconds (0–59). */
  seconds: number;
}

/** ISO-8601 week number and its week-numbering year (which may differ from the calendar year). */
interface IsoWeek {
  /** ISO week number (1–53). */
  week: number;
  /** ISO week-numbering year. */
  week_year: number;
}

/**
 * An immutable date-time — a **canonical UTC instant** with chainable, snake_case methods. Every
 * operation returns a new value; the receiver is never mutated. A zoned *view* from
 * {@link in_zone} re-interprets components, period boundaries, and formatting in an IANA timezone
 * while the underlying instant (and {@link epoch_ms}) stays the same. Serializes to its RFC 3339
 * UTC (`Z`) string inside {@link json} / `JSON.stringify`.
 *
 * @example
 * const due = datetime.parse(ctx.invoiced).add({ months: 1 }).end_of("month");
 * due.in_zone("America/New_York").format("YYYY-MM-DD HH:mm"); // month-end in the customer's zone
 */
interface DateTime {
  /** Calendar year (e.g. `2026`). */
  year(): number;
  /** Month, 1–12. */
  month(): number;
  /** Day of month, 1–31. */
  day(): number;
  /** Hour, 0–23. */
  hour(): number;
  /** Minute, 0–59. */
  minute(): number;
  /** Second, 0–59. */
  second(): number;
  /** Millisecond, 0–999. */
  millisecond(): number;
  /** ISO weekday: 1 = Monday … 7 = Sunday. */
  weekday(): number;
  /** Calendar quarter, 1–4. */
  quarter(): number;
  /** Day of the year, 1–366. */
  day_of_year(): number;
  /** ISO-8601 week `{ week, week_year }`. */
  iso_week(): IsoWeek;
  /** Number of days in this value's month (28–31). */
  days_in_month(): number;
  /** A new instant shifted forward by `delta` (calendar `years`/`months` clamp end-of-month). */
  add(delta: DateTimeDelta): DateTime;
  /** A new instant shifted backward by `delta`. */
  sub(delta: DateTimeDelta): DateTime;
  /** The signed gap `this - other` broken into fields (accepts a value or epoch millis). */
  diff(other: DateTimeInput): DateTimeDiff;
  /** The signed count of whole `unit`s in `this - other` (truncated toward zero). */
  diff_in(other: DateTimeInput, unit: DateTimeDiffUnit): number;
  /** The first instant of the `unit` containing this value (in the view zone; weeks start Monday). */
  start_of(unit: DateTimeUnit): DateTime;
  /** The last instant of the `unit` containing this value (in the view zone). */
  end_of(unit: DateTimeUnit): DateTime;
  /** `true` on a Saturday or Sunday (in the view zone). */
  is_weekend(): boolean;
  /** `true` on a weekday (Mon–Fri); holidays are **not** considered. */
  is_business_day(): boolean;
  /** Shifts by `n` business days, skipping weekends (negative `n` goes backward). */
  add_business_days(n: number): DateTime;
  /** Compares by instant: `-1` if `this < other`, `0` if equal, `1` if greater. */
  cmp(other: DateTimeInput): number;
  /** `this === other` by instant. */
  eq(other: DateTimeInput): boolean;
  /** `this < other` by instant. */
  lt(other: DateTimeInput): boolean;
  /** `this <= other` by instant. */
  lte(other: DateTimeInput): boolean;
  /** `this > other` by instant. */
  gt(other: DateTimeInput): boolean;
  /** `this >= other` by instant. */
  gte(other: DateTimeInput): boolean;
  /** `lo <= this <= hi`, inclusive, by instant. */
  is_between(lo: DateTimeInput, hi: DateTimeInput): boolean;
  /**
   * A zoned **view** over the same instant: components, boundaries, and {@link format} / {@link iso}
   * resolve in `zone` (an IANA name like `"America/New_York"`). {@link epoch_ms} is unchanged. An
   * unknown zone throws.
   */
  in_zone(zone: string): DateTime;
  /** RFC 3339 — UTC `Z` by default (or this view's zone), or the given `zone`'s offset. */
  iso(zone?: string): string;
  /** Epoch seconds (floored). */
  unix(): number;
  /** Epoch milliseconds — the canonical value, unaffected by any view zone. */
  epoch_ms(): number;
  /**
   * Formats with locale-neutral **numeric** tokens — `YYYY YY MM DD HH mm ss SSS`; any other
   * character is a literal. No locale-language month/day names. Renders in `zone` (or this view's).
   * @example dt.format("YYYY-MM-DD HH:mm:ss"); // "2026-07-10 13:30:00"
   */
  format(pattern: string, zone?: string): string;
  /** Serializes as the RFC 3339 UTC (`Z`) string inside {@link json} / `JSON.stringify`. */
  toJSON(): string;
  /** The RFC 3339 UTC (`Z`) string. */
  toString(): string;
}

/**
 * The `datetime` factory (always available). Callable as `datetime(input)` (≡ {@link parse}) plus
 * named constructors. Parsing normalizes to a UTC instant; ambiguous locale strings like
 * `"07/10/2026"` are **not** guessed — they throw.
 *
 * @example
 * datetime("2026-07-10T13:30:00Z");        // parse RFC 3339
 * datetime.from({ year: 2026, month: 7, day: 10 }, "Asia/Tokyo"); // parts in a zone
 */
interface DateTimeFactory {
  (input: DateTimeInput): DateTime;
  /** The current instant (UTC). Removed under the deterministic profile. */
  now(): DateTime;
  /** Parses an RFC 3339 / `YYYY-MM-DD` string, epoch millis, or a {@link DateTime}. Throws on bad input. */
  parse(input: DateTimeInput): DateTime;
  /** Builds an instant from calendar {@link DateTimeParts}, interpreted in `zone` (else UTC). */
  from(parts: DateTimeParts, zone?: string): DateTime;
}

/** Date-time factory (immutable UTC instants + zoned views). Always available. */
declare const datetime: DateTimeFactory;

// ─────────────────────────────────────────────────────────────────────────────
// text — immutable string value-util (Pythonic names, JS semantics). Always on.
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link Text.mask} / {@link Text.redact}. */
interface TextMaskOptions {
  /** Number of trailing characters to leave visible (default 4). */
  keep?: number;
  /** Single character to mask with (default `"*"`). */
  char?: string;
}

/** Options for {@link Text.truncate}. */
interface TextTruncateOptions {
  /** Marker appended when truncation happens; counts toward the limit (default `"…"`). */
  ellipsis?: string;
}

/**
 * An immutable string value with a Python-flavored, snake_case surface. Method NAMES rename native
 * JS string operations; SEMANTICS are JavaScript's — counting and width are UTF-16 code units, and
 * casing is Unicode-default (locale-independent, hence deterministic). Content transforms return a
 * new `Text`; `split`/predicates/`count`/`len` return plain values. Unwrap with `.value`; it also
 * coerces to a plain string via `String(...)` and serializes as a plain string in `json()`.
 */
interface Text {
  /** The underlying plain string. */
  readonly value: string;

  // case
  /** Lowercase (Unicode-default). */
  lower(): Text;
  /** Uppercase (Unicode-default). */
  upper(): Text;
  /** Uppercase the first character, lowercase the rest. */
  capitalize(): Text;
  /** Capitalize the first character of each whitespace-separated word. */
  title(): Text;
  /** Swap the case of each ASCII letter. */
  swap_case(): Text;

  // strip (optional set of characters to strip, else whitespace)
  /** Strip `chars` (or whitespace) from both ends. */
  strip(chars?: string): Text;
  /** Strip `chars` (or whitespace) from the left. */
  lstrip(chars?: string): Text;
  /** Strip `chars` (or whitespace) from the right. */
  rstrip(chars?: string): Text;

  // prefix / suffix
  /** Whether the string starts with `prefix`. */
  starts_with(prefix: string | Text): boolean;
  /** Whether the string ends with `suffix`. */
  ends_with(suffix: string | Text): boolean;
  /** Remove `prefix` if present (else unchanged). */
  removeprefix(prefix: string | Text): Text;
  /** Remove `suffix` if present (else unchanged). */
  removesuffix(suffix: string | Text): Text;

  /** Replace ALL occurrences of `old` with `neu` (Python `str.replace` semantics). */
  replace(old: string | Text, neu: string | Text): Text;
  /** Count non-overlapping occurrences of `sub` (an empty needle yields `len + 1`). */
  count(sub: string | Text): number;

  // splitting (returns plain strings)
  /** Split on `sep`; optional `maxsplit` keeps the remainder in the final piece. */
  split(sep: string | Text, maxsplit?: number): string[];
  /** Split on `sep` from the right; optional `maxsplit` keeps the remainder in the first piece. */
  rsplit(sep: string | Text, maxsplit?: number): string[];
  /** Split into lines on `\n` / `\r` / `\r\n`. */
  splitlines(): string[];

  // padding / alignment (width is bounded; an oversize width throws)
  /** Left-pad with `"0"` to `width`, honoring a leading sign. */
  zfill(width: number): Text;
  /** Left-justify to `width`, padding on the right with `fill` (default space). */
  ljust(width: number, fill?: string): Text;
  /** Right-justify to `width`, padding on the left with `fill` (default space). */
  rjust(width: number, fill?: string): Text;
  /** Center within `width`, padding both sides with `fill` (default space). */
  center(width: number, fill?: string): Text;

  // character-class predicates (false for the empty string)
  /** Whether every character is a Unicode decimal digit. */
  is_digit(): boolean;
  /** Whether every character is a Unicode letter. */
  is_alpha(): boolean;
  /** Whether every character is a Unicode letter or decimal digit. */
  is_alnum(): boolean;
  /** Whether every character is whitespace. */
  is_space(): boolean;

  // ERP shaping verbs
  /** Fold diacritics and slugify to a lowercase `a-z0-9` kebab string (non-latin scripts dropped). */
  slugify(): Text;
  /** Mask all but a kept tail — lossy display, not reversible encoding. */
  mask(opts?: TextMaskOptions): Text;
  /** Alias of {@link Text.mask}. */
  redact(opts?: TextMaskOptions): Text;
  /** Trim and collapse internal whitespace runs to single spaces. */
  collapse(): Text;
  /** Shorten to at most `limit` code units, appending the ellipsis marker when truncated. */
  truncate(limit: number, opts?: TextTruncateOptions): Text;

  // length + interop
  /** Length in UTF-16 code units. */
  len(): number;
  /** The underlying plain string. */
  to_string(): string;
  toString(): string;
  valueOf(): string;
  toJSON(): string;
}

/** String value-util factory. `text(input)` wraps any value as an immutable string. Always available. */
interface TextFactory {
  (input: unknown): Text;
}

/** Immutable string value-util (Pythonic names, JS semantics). Always available. */
declare const text: TextFactory;

// ─────────────────────────────────────────────────────────────────────────────
// Response `meta` — per-capability op metrics keyed by name (`meta.io.<name>`)
// ─────────────────────────────────────────────────────────────────────────────

/** One capability operation's metric. Shape varies by capability; a fragment may narrow it. */
type IoMetric = Record<string, unknown>;

/**
 * The `meta` object the server attaches to every response. `io` carries one entry per capability
 * the request actually used, keyed by name (`meta.io.http`, `meta.io.<resource-name>`, …), each an
 * array of that capability's per-operation metrics. Empty capabilities are omitted.
 */
interface ResponseMeta {
  /** Correlation id — also logged server-side with the raw cause. */
  trace_id: string;
  /** Registered-script key, echoed back in key mode. */
  key?: string;
  /** Partition key, echoed back when supplied. */
  partition?: string;
  /** Script size in bytes. */
  script_bytes: number;
  /** Context payload size in bytes. */
  context_bytes: number;
  /** Total input size in bytes (script + context). */
  total_input_bytes: number;
  /** Execution time in microseconds. */
  exec_time_us: number;
  /** Per-capability operation metrics, keyed by capability name. */
  io: Record<string, IoMetric[]>;
}
// ─────────────────────────────────────────────────────────────────────────────
// Batch wire envelope — the `POST /batch` request/response shapes (client-side types)
// ─────────────────────────────────────────────────────────────────────────────

/** One item of a {@link BatchRequest}: the single-execute body plus an optional client `id`. */
interface BatchItem {
  /** Inline JS source defining `handler(ctx)` (exactly one of `script` / `key`). */
  script?: string;
  /** Registered-script key (exactly one of `script` / `key`). */
  key?: string;
  /** JSON context passed as `ctx` to the handler. */
  context?: unknown;
  /** Per-item config (capabilities, `io`) — same shape as `/execute`. */
  config?: unknown;
  /** Optional client correlation id, echoed on the matching result for subset-retry. */
  id?: string;
}

/** The `POST /batch` request body: an ordered list of independent items. */
interface BatchRequest {
  /** The items to execute (bounded by `batch.max_items` / `batch.max_input_bytes`). */
  items: BatchItem[];
}

/** One entry of {@link BatchResponse.results}: the single-execute envelope plus the echoed `id`. */
interface BatchResultItem {
  /** The item's success payload, or `null` on a system error. */
  data: unknown;
  /** The item's error (application error from `json(...)`, or a structured system envelope). */
  error: unknown;
  /** The item's metadata (same shape as a single response's `meta`). */
  meta: ResponseMeta;
  /** The client id from the corresponding request item, when supplied. */
  id?: string;
}

/** Batch-level summary attached to a {@link BatchResponse}. */
interface BatchMeta {
  /** Number of items in the batch. */
  items: number;
  /** Items that executed successfully. */
  ok: number;
  /** Items that failed (rejected, engine error, or truncated). */
  failed: number;
  /** Wall-clock duration of the whole batch, milliseconds. */
  duration_ms: number;
  /** Batch correlation id (shared by every item's `meta.trace_id`). */
  trace_id: string;
}

/**
 * The `POST /batch` response: order-preserving per-item envelopes plus a batch summary. An admitted
 * batch is always HTTP 200; per-item failures live in `results[i].error`.
 */
interface BatchResponse {
  /** One envelope per request item, in request order. */
  results: BatchResultItem[];
  /** Batch-level summary. */
  meta: BatchMeta;
}
// ─────────────────────────────────────────────────────────────────────────────
// `$sys` — runtime stdlib: crypto (always on); env/secrets when config.sys set
// ─────────────────────────────────────────────────────────────────────────────

/** HMAC hash algorithm. */
type SysHmacAlgo = "sha256" | "sha512";

/** Output encoding for an HMAC digest. */
type SysEncoding = "hex" | "base64" | "base64url";

/** A reversible encode/decode pair (UTF-8 string ⇄ encoded string). */
interface SysCodec {
  /** Encodes a UTF-8 string. */
  encode(input: string): string;
  /** Decodes back to a UTF-8 string (throws on invalid input). */
  decode(input: string): string;
}

/**
 * Pure crypto + encoding helpers (always available). Hashing/HMAC are one-way;
 * the codecs are reversible. A {@link SysSecret} handle may be passed **only** as the
 * `key` of {@link hmac} — every other helper takes a plain `string` and so rejects it.
 */
interface SysCrypto {
  /** SHA-256 of `data`, hex-encoded. */
  sha256(data: string): string;
  /** SHA-512 of `data`, hex-encoded. */
  sha512(data: string): string;
  /**
   * HMAC of `msg` under `key`, `encoding`-encoded (default `"hex"`). `key` may be a
   * {@link SysSecret} handle — it is resolved server-side; the plaintext never enters JS.
   * @example $sys.crypto.hmac("sha256", $sys.secrets.SIGNING_KEY, body);
   */
  hmac(
    algo: SysHmacAlgo,
    key: string | SysSecret,
    msg: string,
    encoding?: SysEncoding,
  ): string;
  /** A time-ordered v7 UUID (millisecond timestamp prefix + random tail). */
  uuid(): string;
  /** Standard base64 codec. */
  base64: SysCodec;
  /** URL-safe base64 (no padding) codec. */
  base64url: SysCodec;
  /** Hex codec. */
  hex: SysCodec;
  /** Percent-encoding (URL escape) codec. */
  url: SysCodec;
}

/**
 * An opaque secret handle from {@link Sys.secrets}. The plaintext **never enters JS** —
 * pass it as the `key` of {@link SysCrypto.hmac}; any coercion (`String(x)`, a template
 * literal, `JSON.stringify`) yields `"[secret:NAME]"`, never the value.
 */
interface SysSecret {
  /** Yields `"[secret:NAME]"` — never the plaintext. */
  toString(): string;
  /** Yields `"[secret:NAME]"` — never the plaintext. */
  toJSON(): string;
}

/**
 * The `$sys` runtime standard library. `crypto` is pure and **always** available; `env` and
 * `secrets` are populated only when `config.sys` is supplied (otherwise they are empty objects).
 * Date/time lives in the top-level {@link datetime} value-util, not here.
 */
interface Sys {
  /** Pure crypto + encoding (always available). */
  crypto: SysCrypto;
  /**
   * Plain, returnable operator config values from `config.sys.env`. Typed as possibly
   * `undefined` so you can probe optional keys (`$sys.env.FLAG === undefined`); a key
   * you know is set can be used directly.
   */
  env: { readonly [key: string]: string | undefined };
  /**
   * Opaque secret handles from `config.sys.secrets` (see {@link SysSecret}). Typed as
   * always-present (you reference the keys you provisioned) so a handle drops straight
   * into {@link SysCrypto.hmac} without a null check; an unprovisioned key is `undefined`
   * at runtime.
   */
  secrets: { readonly [key: string]: SysSecret };
}

/** Runtime stdlib. `$sys.crypto` always available; `env` / `secrets` need `config.sys`. */
declare const $sys: Sys;

// ─────────────────────────────────────────────────────────────────────────────
// `hasura/client` — injectable module: GraphQL client over `api` (not a global)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Type surface for the operator-deployed injectable module `modules/hasura/client.mjs`.
 * Unlike the capability globals above, this is **imported**, not ambient:
 * `import { hasura } from "hasura/client";`. It resolves only when the file is deployed
 * under `config.modules_dir`; see {@link https://github.com/hlop3z/runlet-js/blob/main/docs/modules.md modules.md}.
 */
declare module "hasura/client" {
  /** Options for {@link hasura}. */
  interface HasuraOptions {
    /** Base URL; defaults to `$sys.env.HASURA_ENDPOINT`. */
    endpoint?: string;
    /** End-user JWT → `Authorization: Bearer`, so Hasura enforces row-level permissions. */
    token?: string;
    /** Admin secret; defaults to `$sys.env.HASURA_ADMIN_SECRET`. Ignored when `token` is set. */
    adminSecret?: string;
    /** Sent as `x-hasura-role` to select a permission role. */
    role?: string;
    /** Extra headers merged onto every request. */
    headers?: HttpHeaders;
  }

  /** A GraphQL error entry from Hasura. */
  interface HasuraError {
    /** Human-readable message. */
    message: string;
    /** Hasura error extensions (e.g. `{ code: "validation-failed" }`). */
    extensions?: Record<string, unknown>;
  }

  /** Hasura's raw response envelope from {@link HasuraClient.raw}. */
  interface HasuraEnvelope<T = any> {
    /** The operation result (absent when only `errors` are present). */
    data?: T;
    /** GraphQL/transport errors (absent on full success). */
    errors?: HasuraError[];
  }

  /** A Hasura client bound to a set of credentials / a role. */
  interface HasuraClient {
    /**
     * Runs a GraphQL query and returns **only** `data`. Throws on a GraphQL or transport
     * error (the Error carries `.graphql` = the errors array, `.code` = the first code).
     * @example const d = h.query(`query ($id: uuid!){ users_by_pk(id:$id){ email } }`, { id });
     */
    query<T = any>(query: string, variables?: Record<string, unknown>): T;
    /** Same wire call as {@link query} — named for mutations so call sites read right. */
    mutate<T = any>(query: string, variables?: Record<string, unknown>): T;
    /**
     * Runs a GraphQL operation and returns Hasura's raw `{ data?, errors? }` envelope.
     * Never throws on a GraphQL-level error — inspect `.errors` inline.
     */
    raw<T = any>(
      query: string,
      variables?: Record<string, unknown>,
    ): HasuraEnvelope<T>;
  }

  /** Creates a {@link HasuraClient}. Reads `$sys.env` for defaults when options are omitted. */
  export function hasura(opts?: HasuraOptions): HasuraClient;
  export default hasura;
}

// ─────────────────────────────────────────────────────────────────────────────
// `http` — SSRF-guarded HTTP client (present when `config.allowed_hosts` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** Request/response header map. */
interface HttpHeaders {
  [name: string]: string;
}

/** Query-string parameters for `http.get` (values are stringified). */
interface QueryParams {
  [name: string]: string | number | boolean;
}

/** Result of an `http.*` call. */
interface ApiResponse<T = any> {
  /** HTTP status code, or `0` if the request failed before a response (transport error). */
  status: number;
  /** Parsed JSON body (raw string if not JSON). Present on any HTTP response; absent on a transport failure. */
  data?: T;
  /**
   * In-band transport error — present only when `status === 0` (the request never reached
   * a response). `http` never throws (§13): inspect this inline instead of `try/catch`.
   */
  error?: ApiTransportError;
}

/** Structured transport error on an `http.*` call (`status: 0`). */
interface ApiTransportError {
  /** Stable code: `HTTP_TIMEOUT` | `HTTP_CONNECT` | `HTTP_SSRF_BLOCKED` | `HTTP_BODY_TOO_LARGE` | `HTTP_OP_LIMIT` | `HTTP_ERROR`. */
  code: string;
  /** `true` ⇒ a retry may succeed (transient). */
  retryable: boolean;
  /** Who should act: `"operator"` (network/upstream) or `"developer"` (e.g. blocked host). */
  owner: string;
  /** Always `"api"`. */
  source: string;
  /** Human-safe cause. */
  message?: string;
}

/**
 * HTTP client whose targets are **script-controlled**, so it is SSRF-guarded:
 * only `http`/`https`, the host must be in `config.allowed_hosts`, and
 * private/internal IPs are blocked (re-validated across redirects).
 */
interface HttpClient {
  /**
   * `GET url`, with optional query params appended.
   * @example http.get("https://api.example.com/items", { page: 2 });
   */
  get<T = any>(
    url: string,
    params?: QueryParams,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `POST url` with a JSON `body`. */
  post<T = any>(
    url: string,
    body?: unknown,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `PUT url` with a JSON `body`. */
  put<T = any>(
    url: string,
    body?: unknown,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `PATCH url` with a JSON `body`. */
  patch<T = any>(
    url: string,
    body?: unknown,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `DELETE url`. */
  delete<T = any>(url: string, headers?: HttpHeaders): ApiResponse<T>;
}

/** HTTP client. Present only when `config.allowed_hosts` is non-empty. */
declare const http: HttpClient;

// ─────────────────────────────────────────────────────────────────────────────
// `s3` — presigned URLs + folder usage (present when `config.s3` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** HTTP method a presigned URL is signed for. */
type S3Method = "PUT" | "GET" | "HEAD" | "DELETE";

/** Options for {@link S3.upload_url} / {@link S3.download_url}. */
interface S3PresignOptions {
  /** Object key (path within the bucket), e.g. `"uploads/photo.jpg"`. */
  key: string;
  /** Link lifetime in seconds. Defaults to `config.s3.expires`; capped at `max_expires`. */
  expires?: number;
}

/** Options for the general {@link S3.sign_url}. */
interface S3PresignGeneralOptions extends S3PresignOptions {
  /** HTTP method to sign for. Defaults to `"PUT"`. */
  method?: S3Method;
}

/** Result of {@link S3.sign_url} / {@link S3.upload_url} / {@link S3.download_url}. */
interface S3PresignResult {
  /** The signed URL the browser uses directly. */
  url: string;
  /** The method the URL is signed for. */
  method: S3Method;
  /** The link's lifetime in seconds. */
  expires: number;
}

/** Options for {@link S3.upload_form}. */
interface S3PresignPostOptions {
  /** Object key the upload will be stored under. */
  key: string;
  /** Link lifetime in seconds. Defaults to `config.s3.expires`. */
  expires?: number;
}

/**
 * Result of {@link S3.upload_form} — a browser POST policy whose size limit the
 * object store enforces (the cap comes from `config.s3.max_upload_size`).
 */
interface S3PresignPostResult {
  /** The POST target URL. */
  url: string;
  /** Form fields to send before the `file` part. */
  fields: { [field: string]: string };
  /** The enforced maximum object size in bytes. */
  max_bytes: number;
  /** The policy's lifetime in seconds. */
  expires: number;
}

/** Options for {@link S3.usage}. */
interface S3UsageOptions {
  /** Key prefix to total, e.g. `"user-a/"`. Omit to total the whole bucket. */
  prefix?: string;
}

/** Result of {@link S3.usage}. */
interface S3UsageResult {
  /** The prefix that was totalled (empty string = whole bucket). */
  prefix: string;
  /** Total size in bytes of all objects under the prefix. */
  bytes: number;
  /** Number of objects under the prefix. */
  objects: number;
}

/** Options for {@link S3.delete}. */
interface S3DeleteOptions {
  /** Object key to delete, e.g. `"user-a/photo.jpg"`. */
  key: string;
}

/** Result of {@link S3.delete}. */
interface S3DeleteResult {
  /** The key that was deleted. */
  key: string;
  /** Always `true` on success (S3 delete is idempotent — a missing key still succeeds). */
  deleted: boolean;
}

/**
 * S3-compatible storage helper for `config.s3` (AWS S3, Cloudflare R2, MinIO,
 * Backblaze B2, …). Signing a URL is pure crypto — the server never touches your
 * files; `usage` and `delete` are the calls that connect. The `endpoint` is
 * operator-config and SSRF-guarded. The sign helpers / `delete` throw on an empty `key`.
 */
interface S3 {
  /** Signs a `PUT` upload link. */
  upload_url(opts: S3PresignOptions): S3PresignResult;
  /** Signs a `GET` download link. */
  download_url(opts: S3PresignOptions): S3PresignResult;
  /** Signs a size-limited browser POST upload form (cap from `config.s3.max_upload_size`). */
  upload_form(opts: S3PresignPostOptions): S3PresignPostResult;
  /** Signs a URL for any `method` (default `"PUT"`). `DELETE` needs `config.s3.allow_delete`. */
  sign_url(opts: S3PresignGeneralOptions): S3PresignResult;
  /**
   * Totals the bytes and object count under a key prefix by listing the bucket.
   * No native "folder size" exists in S3, so this walks every object under the
   * prefix; each 1000-object page counts against `max_ops`.
   * @example const u = s3.usage({ prefix: "user-a/" }); // { prefix, bytes, objects }
   */
  usage(opts?: S3UsageOptions): S3UsageResult;
  /**
   * Deletes one object. **Destructive and opt-in** — throws unless the operator
   * set `config.s3.allow_delete = true`, even when `s3` is otherwise configured.
   * @example const d = s3.delete({ key: "user-a/old.jpg" }); // { key, deleted: true }
   */
  delete(opts: S3DeleteOptions): S3DeleteResult;
}

/** S3 storage helper. Present only when `config.s3` is supplied. */
declare const s3: S3;
