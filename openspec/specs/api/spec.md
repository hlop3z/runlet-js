# api Specification

## Purpose

The `api` capability gives a sandboxed handler a controlled outbound HTTP client
(`api.get/post/put/patch/delete`) for calling other services. Because the request URL is
**script-controlled**, `api` follows the SSRF-guarded trust model: every target is checked
against an operator-supplied `allowed_hosts` allowlist and against private/internal IP
blocking. It is opt-in per request, never throws on HTTP/transport failures (errors come back
in-band), and meters every call into `meta.http_requests`. Source of truth: `src/http.rs`,
`src/js/api.js`, `docs/02-api.md`.

## Requirements

### Requirement: Opt-in injection via allowed_hosts

The `api` global SHALL be injected only when the request `config.allowed_hosts` is present and
non-empty; otherwise the `api` global SHALL NOT exist in the handler scope.

#### Scenario: Capability absent without config

- **WHEN** a request omits `config.allowed_hosts` or supplies an empty list
- **THEN** the handler observes `typeof api === "undefined"`

#### Scenario: Capability present with config

- **WHEN** a request supplies a non-empty `config.allowed_hosts`
- **THEN** the handler can call `api.get/post/put/patch/delete`

### Requirement: HTTP method surface

The `api` global SHALL expose `get(url, params, headers)`, `post(url, body, headers)`,
`put(url, body, headers)`, `patch(url, body, headers)`, and `delete(url, headers)`, where
`params`, `body`, and `headers` are optional.

#### Scenario: GET serializes params into the query string

- **WHEN** a handler calls `api.get(url, { page: 1 })`
- **THEN** the request URL has `?page=1` appended (URL-encoded), and additional params are joined with `&`

#### Scenario: Body-bearing methods send a JSON body

- **WHEN** a handler calls `api.post(url, body)` with a non-null body
- **THEN** the body is JSON-serialized and sent with `Content-Type: application/json`

### Requirement: Host allowlist enforcement

The system SHALL reject any request whose target host is not in `allowed_hosts`, matching
case-insensitively, with the literal `"*"` entry permitting any host.

#### Scenario: Disallowed host blocked

- **WHEN** a handler requests a host that is not in `allowed_hosts` (and the list is not `"*"`)
- **THEN** the call is blocked and returns an in-band error with code `HTTP_SSRF_BLOCKED`

#### Scenario: Wildcard allows any host

- **WHEN** `allowed_hosts` contains `"*"`
- **THEN** any otherwise-valid host passes the allowlist check

### Requirement: Scheme allowlist

The system SHALL accept only `http` and `https` request URLs, and SHALL enforce this on the
initial URL and on every redirect hop. A URL or redirect target using any other scheme
(`file`, `gopher`, `ftp`, `data`, …) SHALL be refused deterministically with an in-band
`HTTP_SSRF_BLOCKED` error, independent of whether the underlying client supports the scheme.

#### Scenario: Non-http scheme rejected up front

- **WHEN** a handler requests a URL whose scheme is not `http` or `https`
- **THEN** the call is blocked and returns an in-band error with code `HTTP_SSRF_BLOCKED`, and no connection is attempted

#### Scenario: Cross-protocol redirect rejected

- **WHEN** an allowed `https` host responds with a redirect to a non-http(s) scheme
- **THEN** the redirect is not followed (the scheme is re-checked on the hop, not only on the first request)

### Requirement: Private/internal IP blocking

The system SHALL block requests that resolve to private or internal IP addresses, unless the
server runs in debug mode (which relaxes the private-IP check). The blocked set SHALL cover, in
addition to loopback/private/link-local/CGNAT/unspecified/broadcast: IPv6 loopback and
unspecified, unique-local (`fc00::/7`) and link-local (`fe80::/10`), **all** IPv6 forms embedding a
private IPv4 — IPv4-mapped (`::ffff:a.b.c.d`), the deprecated IPv4-compatible form (`::a.b.c.d`),
6to4 (`2002:…`), and NAT64 (`64:ff9b::…`) — and multicast (`224.0.0.0/4`, `ff00::/8`) and
reserved/future (`240.0.0.0/4`) ranges. Every literal SHALL be canonicalized before classification
so alternate encodings (decimal, octal, hex, short-form) of a blocked address cannot bypass the
check. The address the client connects to SHALL be the address the classifier validated
(connect-time pinning), so a hostname cannot resolve public for validation and private for the
connection.

#### Scenario: Private/internal target blocked

- **WHEN** a handler requests a host resolving to a private/internal IP and the server is not in debug mode
- **THEN** the call is blocked and returns an in-band error with code `HTTP_SSRF_BLOCKED`

#### Scenario: Alternate-encoded internal address blocked

- **WHEN** a handler targets an internal IP written in decimal, octal, hex, or short form (e.g. `2130706433`, `0x7f000001`, `127.1`)
- **THEN** it is canonicalized and blocked exactly as the dotted-quad form would be

#### Scenario: IPv6-wrapped private IPv4 blocked

- **WHEN** a handler targets a private IPv4 smuggled through an IPv6 wrapper (IPv4-mapped, IPv4-compatible, 6to4, or NAT64)
- **THEN** the embedded address is unwrapped and the request is blocked

#### Scenario: Connect-time pinning defeats DNS rebinding

- **WHEN** a hostname resolves to a public address at validation time but to a private address at connection time
- **THEN** the connection is refused because the client connects only to the classifier-validated address, not a re-resolved one

#### Scenario: Debug mode relaxes private-IP block

- **WHEN** the server runs in debug mode
- **THEN** the private/internal-IP check is skipped while the `allowed_hosts` allowlist still applies (subject to the production boot guard)

### Requirement: Debug and wildcard relaxations are production-gated

The system SHALL refuse to start when the private-IP relaxation (`allow_private`, debug mode) or a
wildcard `*` host allowlist is active on a non-loopback bind, unless network isolation is explicitly
asserted in config — because in that configuration the IP classifier is the sole remaining SSRF
defense.

#### Scenario: Relaxed guard on a public bind refuses to boot

- **WHEN** the server is configured with `allow_private` (debug) or a wildcard `*` allowlist on a non-loopback bind and network isolation is not asserted
- **THEN** startup fails with a configuration error rather than serving with the SSRF guard relaxed

#### Scenario: Local development is unaffected

- **WHEN** the same relaxation is used on a loopback bind, or network isolation is explicitly asserted
- **THEN** the server starts normally (local testing and infra-firewalled deployments keep working)

### Requirement: Redirect re-validation

The system SHALL re-validate every redirect hop against the same scheme allowlist, `allowed_hosts`
allowlist, and private/internal-IP rules, and SHALL stop following after at most 5 redirects.

#### Scenario: Redirect to a disallowed host or IP is not followed

- **WHEN** a response redirects to a host not in `allowed_hosts` or to a blocked private/internal IP
- **THEN** the redirect is not followed

#### Scenario: Redirect to a non-http(s) scheme is not followed

- **WHEN** a response redirects to a scheme other than `http`/`https`
- **THEN** the redirect is not followed

#### Scenario: Redirect cap enforced

- **WHEN** a request would exceed 5 redirect hops
- **THEN** redirect following stops

### Requirement: Never-throw, in-band error model

Transport, SSRF, op-limit, and body-size failures SHALL be returned in-band as a value the
handler inspects, not thrown — distinct from the trusted capabilities that throw.

#### Scenario: Transport failure returned in-band

- **WHEN** an HTTP request fails at the transport layer (timeout, connect error)
- **THEN** the call returns `{ status: 0, error: {...} }` (e.g. code `HTTP_TIMEOUT` or `HTTP_CONNECT`) rather than throwing

#### Scenario: Successful response is reshaped

- **WHEN** an HTTP request completes
- **THEN** the result is `{ status, data }`, where `data` is the parsed JSON body (or the raw string if not JSON)

#### Scenario: HTTP error status is not an exception

- **WHEN** a server replies with a non-2xx status (e.g. 404, 500)
- **THEN** the call returns that `status` with the body in `data`, without throwing

### Requirement: Response size and protected headers

The system SHALL reject response bodies larger than the configured cap and SHALL strip
user-supplied protected headers.

#### Scenario: Oversized response rejected

- **WHEN** a response body exceeds the maximum size (10 MiB)
- **THEN** the call returns an in-band error with code `HTTP_BODY_TOO_LARGE`

#### Scenario: Protected headers cannot be overridden

- **WHEN** a handler supplies headers including `content-type`, `content-length`, `host`, or `transfer-encoding` (case-insensitive)
- **THEN** those headers are dropped and not sent as user-controlled values

### Requirement: Operation cap

Each `api` call SHALL count against the per-execution `max_ops` budget, and a call made after
the budget is exhausted SHALL fail.

#### Scenario: Op budget exhausted

- **WHEN** a handler issues an `api` call after `max_ops` external operations are already used
- **THEN** the call returns an in-band error with code `HTTP_OP_LIMIT`

### Requirement: Per-call metering

Every `api` call SHALL be recorded and surfaced in `meta.http_requests`, including blocked and
failed calls, without leaking the request path or query.

#### Scenario: Successful call metered

- **WHEN** a handler makes an `api` call
- **THEN** `meta.http_requests` gains an entry with method, host, status, request/response byte sizes, and duration

#### Scenario: Blocked call still metered

- **WHEN** an `api` call is blocked or fails (status reported as 0)
- **THEN** it still appears in `meta.http_requests` with `status` 0, and only the host (no path or query) is recorded
