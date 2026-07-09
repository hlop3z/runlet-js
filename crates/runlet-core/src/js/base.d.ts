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
 * `json`, `$`, `Decimal`, and `$sys.crypto` / `$sys.date` are pure and **always**
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
// `$` / `Decimal` — exact decimal math (always available)
// ─────────────────────────────────────────────────────────────────────────────

/** A value accepted anywhere a decimal is expected. */
type DecimalInput = number | string | Decimal;

/**
 * An exact, arbitrary-precision decimal. JavaScript has no operator overloading,
 * so arithmetic is method-based and **immutable** — every operation returns a new
 * `Decimal`. Backed by the same engine that decodes Postgres `NUMERIC`, so it
 * round-trips DB decimals without precision loss.
 *
 * @example
 * const total = $("0.1").add("0.2");   // exact 0.3, not 0.30000000000000004
 * total.toString();                    // "0.3"
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
  /** Rounds to `places` decimal places (default `0`), half-away-from-zero. */
  round(places?: number): Decimal;
  /**
   * Converts major units to integer minor units: `this * 10^places`, rounded
   * half-away-from-zero to a whole number. `places` is the count of minor-unit
   * digits and defaults to `2` (cents) — pass `0` for yen, `3` for dinars.
   * @example $("19.99").toCents();   // 1999
   * @example $("1.005").toCents();   // 101  (sub-cent rounds half-up)
   */
  toCents(places?: number): Decimal;
  /**
   * Converts integer minor units back to major units: `this / 10^places`, fixed to
   * `places` decimal places. `places` defaults to `2` (cents).
   * @example $(1999).fromCents(); // "19.99"
   */
  fromCents(places?: number): Decimal;
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
  isZero(): boolean;
  /** `true` if the value is less than zero. */
  isNegative(): boolean;
  /** The exact value as a decimal string (e.g. `"19.99"`). */
  toString(): string;
  /** The value as a JS `number` — may lose precision for large/long decimals. */
  toNumber(): number;
  /** Serializes as the exact string value inside {@link json} / `JSON.stringify`. */
  toJSON(): string;
}

/**
 * Creates a {@link Decimal} from a number, string, or another `Decimal`.
 * `$` and `Decimal` are the same function.
 *
 * @example
 * const price = $("19.99").mul(3).round(2); // "59.97"
 */
interface DecimalFactory {
  (value?: DecimalInput): Decimal;
}

/** Exact-decimal factory. Alias of {@link Decimal}. Always available. */
declare const $: DecimalFactory;
/** Exact-decimal factory. Alias of {@link $}. Always available. */
declare const Decimal: DecimalFactory;

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
// `$sys` — runtime stdlib: crypto + date (always on); env/secrets when config.sys set
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
 * A fixed-length duration for {@link SysDate.add} / {@link SysDate.sub}, like Python's
 * `timedelta`. Only constant-length units — no months/years (ambiguous length).
 */
interface SysDuration {
  weeks?: number;
  days?: number;
  hours?: number;
  minutes?: number;
  seconds?: number;
  ms?: number;
}

/** The gap between two dates, from {@link SysDate.diff}. */
interface SysDateDiff {
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

/**
 * An immutable UTC instant. Arithmetic is method-based and returns a new instance;
 * serializes as its RFC 3339 string inside {@link json} / `JSON.stringify`.
 */
interface SysDate {
  /** A new instant shifted forward by `delta`. */
  add(delta: SysDuration): SysDate;
  /** A new instant shifted backward by `delta`. */
  sub(delta: SysDuration): SysDate;
  /** Breakdown of `this - other` (accepts another instant or epoch millis). */
  diff(other: SysDate | number): SysDateDiff;
  /** RFC 3339 string in UTC, e.g. `"2026-06-08T00:00:00Z"`. */
  iso(): string;
  /** Epoch seconds. */
  unix(): number;
  /** Epoch milliseconds (the canonical value). */
  epochMs(): number;
  /** Serializes as {@link iso}. */
  toJSON(): string;
  /** Serializes as {@link iso}. */
  toString(): string;
}

/** Date helpers (always available). Parsing normalizes everything to UTC. */
interface SysDateFactory {
  /** The current instant (UTC). */
  now(): SysDate;
  /**
   * Parses an ISO 8601 / RFC 3339 string (offset-aware), a `YYYY-MM-DD` date, or epoch
   * millis → a UTC {@link SysDate}. Throws on unparseable input.
   * @example $sys.date.parse(ctx.when).add({ days: 3 }).iso();
   */
  parse(input: string | number | SysDate): SysDate;
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
 * The `$sys` runtime standard library. `crypto` and `date` are pure and **always**
 * available; `env` and `secrets` are populated only when `config.sys` is supplied
 * (otherwise they are empty objects).
 */
interface Sys {
  /** Pure crypto + encoding (always available). */
  crypto: SysCrypto;
  /** Date parse + timedelta math (always available). */
  date: SysDateFactory;
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

/** Runtime stdlib. `$sys.crypto` / `$sys.date` always available; `env` / `secrets` need `config.sys`. */
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
