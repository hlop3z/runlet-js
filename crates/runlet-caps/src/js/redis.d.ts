// ─────────────────────────────────────────────────────────────────────────────
// `redis` — key/value store (present when `config.io.redis` names a resource)
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link Redis.set}. */
interface RedisSetOptions {
  /** Time-to-live in seconds (optional). */
  ttl?: number;
}

/**
 * Redis key/value helper over the **operator-defined** logical resource named in
 * `config.io.redis` (trusted, resolved operator-side — no SSRF guard).
 * **Strings in / strings out**: serialize objects yourself (`JSON.stringify`).
 * All calls are synchronous (no `await`), like `db`. A failure to reach Redis throws a
 * retryable `REDIS_CONNECTION` capability error.
 */
interface Redis {
  /** `GET key` — the string value, or `null` if the key is missing. */
  get(key: string): string | null;
  /** `SET key value [EX ttl]` — returns `true`. */
  set(key: string, value: string, opts?: RedisSetOptions): boolean;
  /** `DEL key` — number of keys removed (0 or 1). */
  del(key: string): number;
  /** `INCR key` — the new value. */
  incr(key: string): number;
  /** `EXPIRE key seconds` — `true` if the key existed and the TTL was set. */
  expire(key: string, seconds: number): boolean;
}

/** Redis helper. Present only when `config.io.redis` names a resource. */
declare const redis: Redis;
