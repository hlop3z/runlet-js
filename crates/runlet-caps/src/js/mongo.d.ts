// ─────────────────────────────────────────────────────────────────────────────
// `mongo` — document database (present when `config.io.mongo` names a resource)
// ─────────────────────────────────────────────────────────────────────────────

/** A document, keyed by field name. */
interface MongoDoc {
  [field: string]: any;
}

/** Options for {@link Mongo.find}. */
interface MongoFindOptions {
  /** Maximum documents to return (driver-side). */
  limit?: number;
  /** Documents to skip before returning. */
  skip?: number;
  /** Sort spec, e.g. `{ createdAt: -1 }`. */
  sort?: MongoDoc;
  /** Projection spec, e.g. `{ _id: 0, name: 1 }`. */
  projection?: MongoDoc;
}

/** Result of {@link Mongo.find} / {@link Mongo.aggregate}. */
interface MongoFindResult {
  /** The documents returned (capped by the server's `max_docs`). */
  docs: MongoDoc[];
  /** Number of documents in {@link docs}. */
  count: number;
  /** `true` if documents were dropped because the result hit `max_docs`. */
  truncated: boolean;
}

/** Result of {@link Mongo.insert_one}. */
interface MongoInsertOneResult {
  /** The new document's id as a string (hex for an `ObjectId`). */
  inserted_id: string;
}

/** Result of {@link Mongo.insert_many}. */
interface MongoInsertManyResult {
  /** Number of documents inserted. */
  inserted_count: number;
}

/** Result of {@link Mongo.update_one} / {@link Mongo.update_many}. */
interface MongoUpdateResult {
  /** Documents that matched the filter. */
  matched: number;
  /** Documents actually modified. */
  modified: number;
}

/** Result of {@link Mongo.delete_one} / {@link Mongo.delete_many}. */
interface MongoDeleteResult {
  /** Documents removed. */
  deleted: number;
}

/**
 * Document-database client over an **operator-defined** logical resource named in
 * `config.io.mongo` (connection + credentials resolved operator-side), so it is
 * trusted — no SSRF guard. Synchronous (no `await`), like `db`. **Type fidelity:**
 * values that don't fit a JS number exactly come back as **strings** — `Int64` and
 * `Decimal128` as strings, `ObjectId` as its hex string, `Date` as RFC 3339, `Binary` as
 * base64; `Int32`/`Double` are numbers. Filters/updates/pipelines are passed as data (never
 * string-interpolated). A failure to reach the database throws a retryable `MONGO_CONNECTION`.
 */
interface Mongo {
  /**
   * Finds documents matching `filter`.
   * @example mongo.find("users", { active: true }, { limit: 50, sort: { name: 1 } });
   */
  find(
    collection: string,
    filter?: MongoDoc,
    options?: MongoFindOptions,
  ): MongoFindResult;
  /** Finds the first matching document, or `null`. */
  find_one(collection: string, filter?: MongoDoc): MongoDoc | null;
  /** Counts documents matching `filter`. */
  count(collection: string, filter?: MongoDoc): number;
  /**
   * Runs an aggregation pipeline.
   * @example mongo.aggregate("orders", [{ $group: { _id: "$user", total: { $sum: "$amount" } } }]);
   */
  aggregate(collection: string, pipeline: MongoDoc[]): MongoFindResult;
  /** Inserts one document. */
  insert_one(collection: string, doc: MongoDoc): MongoInsertOneResult;
  /** Inserts many documents. */
  insert_many(collection: string, docs: MongoDoc[]): MongoInsertManyResult;
  /** Updates the first matching document (`update` needs atomic operators like `$set`). */
  update_one(
    collection: string,
    filter: MongoDoc,
    update: MongoDoc,
  ): MongoUpdateResult;
  /** Updates every matching document. */
  update_many(
    collection: string,
    filter: MongoDoc,
    update: MongoDoc,
  ): MongoUpdateResult;
  /** Deletes the first matching document. */
  delete_one(collection: string, filter: MongoDoc): MongoDeleteResult;
  /** Deletes every matching document. */
  delete_many(collection: string, filter: MongoDoc): MongoDeleteResult;
}

/** Document-database client. Present only when `config.io.mongo` names a resource. */
declare const mongo: Mongo;
