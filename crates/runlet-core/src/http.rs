//! Controlled `api` HTTP client for the `QuickJS` sandbox.
//!
//! JS API: `api.get/post/put/patch/delete(url, ...)`
//! Access controlled per-request via `allowed_hosts`.
//! Each call is metered in `HttpMetric` for auditing.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::redirect;
use rquickjs::{Ctx, Function, Value as JsValue};
use serde::Serialize;

use crate::errors::{self, ErrorOwner, Fault};
use crate::sandbox::{self, Collector};
use crate::ssrf::{self, block_private_ip};

/// Timeout for each HTTP request from JS.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum HTTP response body size (10 MiB).
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of redirects to follow per request.
const MAX_REDIRECTS: usize = 5;

/// Headers users cannot override.
const PROTECTED_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "host",
    "transfer-encoding",
];

/// JS wrapper — loaded from `src/js/api.js` at compile time.
const API_WRAPPER: &str = include_str!("js/api.js");

/// Fallback fault for an HTTP transport error with no specific predicate.
const HTTP_FALLBACK: Fault = Fault::new("HTTP_ERROR", true, ErrorOwner::Operator);
/// Per-execution op budget exhausted before the request.
const HTTP_OP_LIMIT: Fault = Fault::new("HTTP_OP_LIMIT", false, ErrorOwner::Developer);
/// URL rejected by the SSRF guard / `allowed_hosts` (deterministic) — the script chose it.
const HTTP_SSRF_BLOCKED: Fault = Fault::new("HTTP_SSRF_BLOCKED", false, ErrorOwner::Developer);
/// Response exceeded the body size cap (deterministic).
const HTTP_BODY_TOO_LARGE: Fault = Fault::new("HTTP_BODY_TOO_LARGE", false, ErrorOwner::Developer);

/// An HTTP error carrying its classified [`Fault`] plus the raw message.
///
/// Unlike `db`/`mail`/`s3`, `api` never throws (§13): the closure turns this into an
/// **in-band** `{ status: 0, error }` value the script inspects.
#[derive(Debug)]
struct HttpError {
    /// Classified code + retry hint.
    fault: Fault,
    /// Raw message.
    message: String,
}

impl HttpError {
    /// Builds an error with an explicit fault.
    const fn new(fault: Fault, message: String) -> Self {
        Self { fault, message }
    }

    /// Classifies a `reqwest` transport error by its predicates.
    fn from_transport(err: &reqwest::Error, method: &str, url: &str) -> Self {
        Self {
            fault: classify(err),
            message: format!("HTTP {method} {url}: {err}"),
        }
    }
}

/// Maps a `reqwest::Error` to a [`Fault`] (docs/99-errors.md).
fn classify(err: &reqwest::Error) -> Fault {
    if err.is_timeout() {
        Fault::new("HTTP_TIMEOUT", true, ErrorOwner::Operator)
    } else if err.is_connect() {
        Fault::new("HTTP_CONNECT", true, ErrorOwner::Operator)
    } else {
        HTTP_FALLBACK
    }
}

/// Metric recorded for each HTTP request.
#[derive(Debug, Clone, Serialize)]
pub struct HttpMetric {
    /// HTTP method.
    method: String,
    /// Host only (no path/query — privacy).
    host: String,
    /// Status code (0 if blocked/failed).
    status: u16,
    /// Request body size in bytes.
    request_bytes: usize,
    /// Response body size in bytes.
    response_bytes: usize,
    /// Duration in microseconds.
    duration_us: u128,
}

impl HttpMetric {
    /// Operation duration in microseconds (for the per-capability latency histogram).
    #[must_use]
    pub const fn duration_us(&self) -> u128 {
        self.duration_us
    }
}

/// Injects the `api` global and returns a metrics collector.
///
/// # Errors
///
/// Returns an error if client creation, registration, or JS eval fails.
pub fn inject_api(
    qctx: &Ctx<'_>,
    allowed_hosts: &[String],
    max_ops: usize,
    allow_private: bool,
    wildcard_allowed: bool,
) -> Result<Collector<HttpMetric>, Box<dyn Error + Send + Sync>> {
    let hosts = Arc::new(allowed_hosts.to_vec());
    let metrics = sandbox::new_collector();

    // Build client with redirect policy that validates each hop.
    let redirect_hosts = Arc::clone(&hosts);
    let policy = redirect::Policy::custom(move |attempt| {
        validate_redirect(&redirect_hosts, attempt, allow_private, wildcard_allowed)
    });
    let client = Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(policy)
        // Pin resolution: the address reqwest connects to is the one the SSRF filter passed,
        // closing the DNS-rebinding TOCTOU window between a pre-check and the connect lookup.
        .dns_resolver(Arc::new(ssrf::SsrfResolver::new(allow_private)))
        .build()?;

    let closure_hosts = Arc::clone(&hosts);
    let closure_metrics = Arc::clone(&metrics);

    let http_fn = Function::new(
        qctx.clone(),
        move |method: String, url: String, body: String, headers_json: String| -> String {
            if let Err(err) = sandbox::check_op_limit(&closure_metrics, max_ops) {
                return errors::api_inband_error_json(HTTP_OP_LIMIT, &err);
            }

            let start = Instant::now();

            let host = match validate_url(&url, &closure_hosts, allow_private, wildcard_allowed) {
                Ok(validated_host) => validated_host,
                Err(err) => {
                    let mctx = MetricCtx {
                        method: &method,
                        host: &extract_host(&url),
                        request_bytes: body.len(),
                        start,
                    };
                    sandbox::record(&closure_metrics, mctx.finish(0, 0));
                    return errors::api_inband_error_json(HTTP_SSRF_BLOCKED, &err);
                }
            };

            let mctx = MetricCtx {
                method: &method,
                host: &host,
                request_bytes: body.len(),
                start,
            };

            let headers = parse_headers(&headers_json);

            match execute_http(&client, &method, &url, &body, &headers) {
                Ok((status, response_bytes, json)) => {
                    sandbox::record(&closure_metrics, mctx.finish(status, response_bytes));
                    json
                }
                Err(http_err) => {
                    sandbox::record(&closure_metrics, mctx.finish(0, 0));
                    errors::api_inband_error_json(http_err.fault, &http_err.message)
                }
            }
        },
    )?
    .with_name("__http")?;

    qctx.globals().set("__http", http_fn)?;

    let wrapper: JsValue<'_> = qctx.eval(API_WRAPPER)?;
    drop(wrapper);

    Ok(metrics)
}

// -- Helpers ----------------------------------------------------------------

/// Context for building an `HttpMetric` with only 3 fields varying.
struct MetricCtx<'a> {
    /// HTTP method.
    method: &'a str,
    /// Host.
    host: &'a str,
    /// Request body size.
    request_bytes: usize,
    /// Timer start.
    start: Instant,
}

impl MetricCtx<'_> {
    /// Builds the final metric with response info.
    fn finish(&self, status: u16, response_bytes: usize) -> HttpMetric {
        HttpMetric {
            method: self.method.into(),
            host: self.host.into(),
            status,
            request_bytes: self.request_bytes,
            response_bytes,
            duration_us: self.start.elapsed().as_micros(),
        }
    }
}

// -- URL validation ---------------------------------------------------------

/// The only request schemes the guard ever permits. Enforced explicitly (not left to the
/// client's supported-scheme set) so a cross-protocol target — `file`, `gopher`, `ftp`,
/// `data`, … — is a deterministic block on the initial URL and on every redirect hop.
const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

/// Returns `true` if the URL scheme is one the guard permits (`http`/`https`). The `url`
/// crate lowercases the scheme during parsing, so an exact compare suffices.
fn is_scheme_allowed(scheme: &str) -> bool {
    ALLOWED_SCHEMES.contains(&scheme)
}

/// Validates URL: checks the scheme allowlist, allowed hosts, and blocks private/internal IPs.
/// Returns the host string (for metrics) on success.
fn validate_url(
    url: &str,
    allowed: &[String],
    allow_private: bool,
    wildcard_allowed: bool,
) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|err| format!("invalid URL '{url}': {err}"))?;

    if !is_scheme_allowed(parsed.scheme()) {
        return Err(format!(
            "scheme '{}' is not allowed (only http/https)",
            parsed.scheme()
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| format!("URL has no host: {url}"))?;

    if !is_host_allowed(host, allowed, wildcard_allowed) {
        return Err(format!("host '{host}' is not in allowed_hosts"));
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    block_private_ip(host, port, allow_private)?;

    Ok(host.into())
}

/// Validates a redirect hop against allowed hosts and private IPs.
fn validate_redirect(
    hosts: &[String],
    attempt: redirect::Attempt<'_>,
    allow_private: bool,
    wildcard_allowed: bool,
) -> redirect::Action {
    if attempt.previous().len() >= MAX_REDIRECTS {
        return attempt.stop();
    }
    let url = attempt.url();
    // Re-check the scheme per hop — a redirect to `file://`/`gopher://`/… is refused here, not
    // left to the client's supported-scheme set.
    if !is_scheme_allowed(url.scheme()) {
        return attempt.stop();
    }
    let host = url.host_str().unwrap_or("");
    if !is_host_allowed(host, hosts, wildcard_allowed) {
        return attempt.stop();
    }
    let port = url.port_or_known_default().unwrap_or(80);
    if block_private_ip(host, port, allow_private).is_err() {
        return attempt.stop();
    }
    attempt.follow()
}

/// Returns `true` if the host is in the allowed list. The wildcard `*` matches every host
/// **only** when `wildcard_allowed` is set — otherwise it is treated as a literal (and so
/// matches nothing), because `*` collapses the host layer down to the IP filter alone and is
/// only safe as an explicit operator opt-in (never in debug/private mode). See
/// `EngineConfig::allow_wildcard_hosts`.
fn is_host_allowed(host: &str, allowed: &[String], wildcard_allowed: bool) -> bool {
    if wildcard_allowed && allowed.iter().any(|ah| ah == "*") {
        return true;
    }
    let host_lower = host.to_lowercase();
    allowed.iter().any(|ah| ah.to_lowercase() == host_lower)
}

/// Extracts the host from a URL (privacy: no path/query). Used for metrics only.
fn extract_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(String::from))
        .unwrap_or_else(|| "unknown".into())
}

// -- Headers ----------------------------------------------------------------

/// Parses user headers JSON, filtering out protected keys (case-insensitive).
fn parse_headers(headers_json: &str) -> BTreeMap<String, String> {
    if headers_json.is_empty() {
        return BTreeMap::new();
    }

    serde_json::from_str::<BTreeMap<String, String>>(headers_json)
        .unwrap_or_default()
        .into_iter()
        .filter(|(key, _)| !PROTECTED_HEADERS.contains(&key.to_lowercase().as_str()))
        .collect()
}

// -- HTTP execution ---------------------------------------------------------

/// Executes an HTTP request. Returns `(status, response_bytes, json_string)`.
fn execute_http(
    client: &Client,
    method: &str,
    url: &str,
    body: &str,
    headers: &BTreeMap<String, String>,
) -> Result<(u16, usize, String), HttpError> {
    let req_method: reqwest::Method = method.parse().map_err(|err| {
        HttpError::new(
            HTTP_FALLBACK,
            format!("invalid HTTP method '{method}': {err}"),
        )
    })?;

    let mut request = client.request(req_method, url);

    for (key, val) in headers {
        request = request.header(key, val);
    }

    if !body.is_empty() {
        request = request
            .header("Content-Type", "application/json")
            .body(body.to_owned());
    }

    let response = request
        .send()
        .map_err(|err| HttpError::from_transport(&err, method, url))?;

    let status = response.status().as_u16();

    // Reject oversized responses before reading into memory.
    if let Some(len) = response.content_length() {
        let size = usize::try_from(len).unwrap_or(usize::MAX);
        if size > MAX_RESPONSE_BYTES {
            return Err(HttpError::new(
                HTTP_BODY_TOO_LARGE,
                format!("response too large: {len} bytes (max {MAX_RESPONSE_BYTES})"),
            ));
        }
    }

    let response_body = response.text().map_err(|err| {
        HttpError::new(
            HTTP_FALLBACK,
            format!("failed to read response body: {err}"),
        )
    })?;

    // Post-read check (Content-Length can lie or be absent).
    if response_body.len() > MAX_RESPONSE_BYTES {
        return Err(HttpError::new(
            HTTP_BODY_TOO_LARGE,
            format!(
                "response too large: {} bytes (max {MAX_RESPONSE_BYTES})",
                response_body.len()
            ),
        ));
    }

    let response_bytes = response_body.len();
    let escaped_body = serde_json::to_string(&response_body).map_err(|err| {
        HttpError::new(
            HTTP_FALLBACK,
            format!("failed to serialize response: {err}"),
        )
    })?;

    Ok((
        status,
        response_bytes,
        format!("{{\"status\":{status},\"body\":{escaped_body}}}"),
    ))
}

#[cfg(test)]
mod tests {
    //! Host-allowlist wildcard gating, the explicit scheme allowlist, and the alt-encoding
    //! canonicalization the guard inherits from the `url` crate (pinned by our own tests).

    use super::{is_host_allowed, is_scheme_allowed, validate_url};
    use crate::ssrf::block_private_ip;

    /// Builds a single-element allow list.
    fn allow(host: &str) -> Vec<String> {
        vec![host.to_owned()]
    }

    /// `*` matches every host only when `wildcard_allowed`; otherwise it's an inert literal.
    #[test]
    fn wildcard_honored_only_when_allowed() {
        assert!(
            is_host_allowed("evil.example", &allow("*"), true),
            "wildcard matches when allowed"
        );
        assert!(
            !is_host_allowed("evil.example", &allow("*"), false),
            "wildcard is inert when not allowed"
        );
    }

    /// An explicit host matches case-insensitively; an unlisted host never matches.
    #[test]
    fn explicit_hosts_match_case_insensitively() {
        assert!(
            is_host_allowed("API.Example.com", &allow("api.example.com"), false),
            "case-insensitive exact match"
        );
        assert!(
            !is_host_allowed("other.example", &allow("api.example.com"), false),
            "unlisted host rejected"
        );
    }

    /// Only `http`/`https` pass the scheme allowlist; every other scheme is rejected.
    #[test]
    fn scheme_allowlist_permits_only_http() {
        assert!(is_scheme_allowed("http"), "http is allowed");
        assert!(is_scheme_allowed("https"), "https is allowed");
        for scheme in ["file", "gopher", "ftp", "data", "ws"] {
            assert!(!is_scheme_allowed(scheme), "{scheme} is rejected");
        }
    }

    /// `validate_url` rejects a non-http(s) URL before any host/IP check — the scheme gate
    /// is deterministic and does not depend on the client's supported schemes.
    #[test]
    fn validate_url_rejects_non_http_scheme() {
        assert!(
            validate_url("file:///etc/passwd", &allow("*"), true, true).is_err(),
            "file:// is refused up front"
        );
        assert!(
            validate_url("gopher://evil.example/", &allow("evil.example"), true, true).is_err(),
            "gopher:// is refused up front"
        );
    }

    /// D4: decimal/octal/hex/short-form IP literals are canonicalized to a dotted quad by the
    /// `url` crate's WHATWG host parser *before* classification, so an internal address written
    /// in an alternate encoding is blocked exactly as the dotted-quad form is. Pinned here so a
    /// future `url`-crate behavior change is caught by our suite, not silently reopened. Uses the
    /// same parse path `validate_url` runs (`reqwest::Url::parse` → `host_str` → `block_private_ip`).
    #[test]
    fn alt_encoded_loopback_canonicalizes_to_blocked() {
        for url in [
            "http://2130706433/", // decimal 127.0.0.1
            "http://0x7f000001/", // hex 127.0.0.1
            "http://127.1/",      // short form 127.0.0.1
            "http://0177.0.0.1/", // octal-first-octet 127.0.0.1
        ] {
            let parsed = reqwest::Url::parse(url)
                .unwrap_or_else(|_err| unreachable!("test URL must parse: {url}"));
            let host = parsed
                .host_str()
                .unwrap_or_else(|| unreachable!("test URL must have a host: {url}"));
            assert_eq!(
                host, "127.0.0.1",
                "{url} canonicalizes to dotted-quad loopback"
            );
            let port = parsed.port_or_known_default().unwrap_or(80);
            assert!(
                block_private_ip(host, port, false).is_err(),
                "{url} resolves to a blocked loopback address"
            );
        }
    }
}
