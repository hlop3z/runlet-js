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
