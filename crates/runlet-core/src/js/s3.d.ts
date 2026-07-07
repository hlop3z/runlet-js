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
