// ─────────────────────────────────────────────────────────────────────────────
// `db` — Postgres / CockroachDB (present when `config.io.db` names a resource)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * A single column value. Values that don't fit a JS number exactly come back as
 * **strings** — `BIGINT` (INT8), `NUMERIC`/`DECIMAL`, `UUID`, timestamps, etc.
 * INT2/INT4 and floats are numbers. Use {@link $} for exact math on string decimals.
 */
type DbValue = string | number | boolean | null;

/** A result row, keyed by column name. */
interface DbRow {
  [column: string]: DbValue;
}

/** Result of a `db.query` call. */
interface DbResult {
  /** Column names, in selection order. */
  columns: string[];
  /** The rows returned (capped by the server's `max_rows`). */
  rows: DbRow[];
  /** Number of rows in {@link rows}. */
  row_count: number;
  /** `true` if rows were dropped because the result hit `max_rows`. */
  truncated: boolean;
}

/** Result of a `db.execute` call. */
interface DbExecResult {
  /** Number of rows the statement inserted, updated, or deleted. */
  rows_affected: number;
}

/**
 * SQL client over an **operator-defined** logical resource named in
 * `config.io.db`, so it is trusted — no SSRF guard. The connection and
 * credentials are resolved operator-side (the egress sidecar); the request only
 * names the resource. Parameters are bound with `$1`, `$2`, … (never string
 * interpolation). Use `query` for statements that return rows and `execute`
 * for writes — they return different shapes.
 */
interface Db {
  /**
   * Runs a SQL statement and returns its rows.
   * @example db.query("SELECT id, email FROM users WHERE id = $1", [ctx.id]);
   */
  query(sql: string, params?: unknown[]): DbResult;
  /**
   * Runs a SQL statement (typically a write) and returns the affected-row count.
   * @example db.execute("UPDATE users SET seen = now() WHERE id = $1", [ctx.id]); // { rows_affected: 1 }
   */
  execute(sql: string, params?: unknown[]): DbExecResult;
  /** Begins a transaction. */
  begin(): void;
  /** Commits the current transaction. */
  commit(): void;
  /** Rolls back the current transaction. */
  rollback(): void;
}

/** SQL client. Present only when `config.io.db` names a resource. */
declare const db: Db;
