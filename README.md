# runlet

A sandboxed JavaScript execution engine built in Rust. Send a JS handler function + context via HTTP, get structured `{data, error, meta}` back.

Powered by QuickJS (via rquickjs), axum, and mimalloc.

> 🧒 **New here?** Start with the friendly, beginner-first guide in **[`docs/`](docs/README.md)** —
> it explains `http`, `db`, `mail`, `s3`, and how to handle money/decimals in plain language.

The driver-backed capabilities are brokered by the **`fabricd` egress sidecar**, which lives
in its own repo: [github.com/hlop3z/fabricd](https://github.com/hlop3z/fabricd). This repo is
fully independent of it — `fabricd` implements the wire contract defined here
(`crates/runlet-wire`) and can be replaced by anything else that speaks it.

## [Docker](https://github.com/hlop3z/runlet-js/pkgs/container/runlet-js)

- [Docker-Compose](container/)

```sh
docker run --rm -it -p 4172:3000 ghcr.io/hlop3z/runlet-js:latest
```

```sh
curl -X POST http://localhost:4172/execute -H "Content-Type: application/json" \
-d '{
  "script": "function handler(ctx) { return json({ greeting: \"hello \" + ctx.name }, null); }",
  "context": { "name": "Alice" }
}'
```

## Quick start

```sh
cargo run
# Server starts on http://127.0.0.1:3000
```

## Endpoint

```
POST /execute
```

### Request

```json
{
  "script": "function handler(ctx) { return json({ greeting: 'hello ' + ctx.name }, null); }",
  "context": { "name": "Alice" },
  "config": {
    "allowed_hosts": ["api.example.com"],
    "io": ["orders", "cache"]
  }
}
```

The box ships **three in-engine built-ins** — `http` (script-controlled URL, SSRF-guarded), `s3`
(pure signing), and `io` (operator-named logical egress). Any other capability (a database, cache,
queue, mail relay, …) is reached through the one primitive `io.call(name, action, payload)` and is
**user-composed** — see the guide [Build your own capability](docs/03-capabilities.md). `config.io`
is a **flat allowlist of logical names** (`["orders","cache"]`); the box is kind-blind and forwards
only the names. Each name resolves either **box-direct** to an operator-declared co-located loopback
endpoint (`local_resources` config) or through a **broker** (the reference `fabricd` sidecar) that
holds the credentials — the runlet server holds no remote endpoint or password. `http`
(`allowed_hosts`) and `s3` stay script-controlled/in-engine and keep their inline config.

| Field                  | Required | Description                                                              |
| ---------------------- | -------- | ------------------------------------------------------------------------ |
| `script`               | one of   | JS source defining a `handler(ctx)` function                             |
| `key`                  | one of   | Registered-script key (see [Registered scripts](#registered-scripts))    |
| `context`              | no       | JSON object passed as `ctx` to the handler                               |
| `config.allowed_hosts` | no       | Hosts the script can reach via `http.*` (`["*"]` = any, `[]` = disabled). A `host:port` entry (e.g. `localhost:8000`) lifts the private-IP block for that exact target only — the production-safe way to reach a co-located service |
| `config.io`            | no       | Flat allowlist of logical resource names, e.g. `["orders","cache"]` — resolved operator-side (box-direct or broker), no creds. A name not listed is rejected `RESOURCE_NOT_FOUND` |
| `config.s3`            | no       | S3/R2/MinIO connection for presigned URLs (in-engine; omit to disable `s3.*`) |
| `config.sys`           | no       | `$sys.env` / `$sys.secrets` context                                      |

### Response

```json
{
  "data": { "greeting": "hello Alice" },
  "error": null,
  "meta": {
    "trace_id": "be04701d-2480-45ec-acb9-787a1be024ba",
    "script_bytes": 82,
    "context_bytes": 16,
    "total_input_bytes": 98,
    "exec_time_us": 950,
    "io": {
      "http": [{ "method": "GET", "host": "api.example.com", "status": 200, "duration_us": 410 }]
    }
  }
}
```

Always `{data, error, meta}`. The handler controls `data` and `error` via the `json()`
bridge. `meta.io` carries one entry per capability the request actually used — `http`, `s3`,
or a logical `io.call` nickname — each an array of that capability's per-operation metrics;
capabilities that made no calls are omitted (so `io` is `{}` for a pure-compute run). On a **system-generated** failure, `error` is a structured envelope —
`{ type, source, code, message, retryable, owner, details?, debug? }` — that a client can
branch on without parsing strings; `meta.trace_id` correlates it with server logs. See
[`docs/99-errors.md`](docs/99-errors.md) for the full contract.

A handler that calls [`emit(kind, value)`](#emitkind-value--tagged-effects) also gets a
top-level `effects` array (`[{ kind, value }]`); it is omitted when nothing was emitted. When the
trusted gateway requests diagnostic capture, a top-level `logs` array
([`log.*`](#log--diagnostic-logging)) is also attached; it is omitted otherwise.

### Batch execution

```
POST /batch
```

Runs many **independent** executions in one round-trip — the fit for per-row ETL, webhook
fan-out, or bulk validation, where a user-space loop inside one handler would share one
sandbox, one wall-clock budget, and one `meta`, so a single bad row kills the whole run.

```json
{
  "items": [
    { "script": "function handler(ctx){ return json(ctx.n * 2, null); }", "context": { "n": 21 }, "id": "row-1" },
    { "key": "billing/price", "context": { "sku": "abc" }, "id": "row-2" }
  ]
}
```

```json
{
  "results": [
    { "data": 42, "error": null, "meta": { "...": "..." }, "id": "row-1" },
    { "data": null, "error": { "code": "..." }, "meta": { "...": "..." }, "id": "row-2" }
  ],
  "meta": { "items": 2, "ok": 1, "failed": 1, "duration_ms": 3, "trace_id": "..." }
}
```

Each item is the same body shape as `/execute` (`script` **xor** `key`, optional `context`
/ `config`) plus an optional client `id` echoed on its result for subset-retry. The
contract:

- **Independent, no atomicity.** Items don't share state, aren't ordered during execution,
  and egress side effects are never rolled back. `results` preserves *request* order; there
  is no `sequential` mode — an ordered multi-step flow belongs in one script.
- **Partial failure is normal.** An admitted batch returns **HTTP 200**; a per-item failure
  lands in `results[i].error` and is counted in `meta.failed`. Only batch-level rejections
  (auth, malformed body, or the caps below) use a non-200 `{data, error, meta}` response.
- **Per item, not per batch.** Each item passes the same validation, admission, per-tenant
  quota, and — in trusted mode — capability authorization as a single request. A batch is N
  requests in cost and accounting, never a way to slip an operation past a per-request gate.
- **Bounded.** `batch.max_items` and `batch.max_input_bytes` bound the request (oversize ⇒
  `400`); `batch.max_response_bytes` bounds the response — an item that would exceed it is
  truncated to a `BATCH_RESPONSE_TRUNCATED` error envelope rather than buffered. A single
  batch can occupy at most its partition's fair share of the runtime pool; the rest queue.

### Logical resources (`config.io`)

Driver-backed capabilities are addressed by **logical name**, not inline connection
config. The runlet server links no database/mail/broker driver and **holds no
credentials** — the operator declares each named resource once in the **`fabricd`
egress sidecar's** config (`fabricd.json`, or the path in `FABRICD_CONFIG`), in its
`resources` table (an internally-tagged object: `kind` selects the capability, the rest
is that driver's config; an optional `tenant` scopes the binding to one workspace in
trusted-identity mode):

```json
{
  "resources": {
    "orders-db": { "kind": "db", "host": "pg", "user": "app", "password": "secret", "database": "shop" },
    "cache":     { "kind": "redis", "url": "redis://cache:6379/0" }
  }
}
```

Alongside `resources`, `fabricd.json` takes the `db` resilience knobs (it owns the driver
connections): `max_statement_timeout_ms` (Tier 0 ceiling clamping any db resource's
`statement_timeout_ms`; `0` = no clamp) and `db_breaker_threshold` /
`db_breaker_cooldown_ms` (Tier 3 per-target circuit breaker — after N consecutive connect
failures to a `host:port`, further `db` calls fast-fail `DB_CIRCUIT_OPEN` for the cool-down
instead of waiting on the connect timeout; `0` = off, cool-down default 5000 ms). See
[`docs/design/resilience.md`](docs/design/resilience.md). An optional `metrics_listen`
(e.g. `"127.0.0.1:9090"`) exposes the daemon's own Prometheus counters —
`fabricd_db_breaker_trips_total` and `fabricd_auth_failures_total` — as a plaintext
`GET /metrics`; omit it for no listener, and never expose it publicly.

The runlet server config carries only the sidecar **transport**: `fabricd_socket` (local
Unix socket, the default deployment) or `fabricd_quic` (remote `fabricd` over QUIC) — see
[Configuration](#configuration). A request then lists, as a **flat allowlist of logical
names**, the resources it may use, and reaches them with `io.call(name, action, payload)`:

```json
{ "config": { "io": ["orders", "cache"] } }
```

The name gates egress: `io.call("orders", …)` is allowed only if `"orders"` is in the
allowlist, else the call is rejected `RESOURCE_NOT_FOUND` before any I/O. A listed name
resolves either **box-direct** — when the operator bound it to a co-located loopback endpoint
in the box's global `local_resources` map — or through the **broker** (`fabricd`), which
resolves the name to a kind/endpoint/credentials. Naming a broker-resolved resource when no
sidecar transport is configured is a `503` `EGRESS_UNAVAILABLE`. This is the trust boundary: a
(possibly compromised) caller can only reach operator-provisioned resources and never sees an
endpoint or a credential. Box-direct bindings are loopback-only (a remote target must go
through a broker; the boot guard refuses a non-loopback binding):

```json
{ "local_resources": { "pricing": { "url": "http://localhost:8080" } } }
```

Both paths carry the identical `{action, payload}` envelope, so a name can move between
box-direct and broker resolution with no script change. Design + the three extension paths:
[`docs/03-capabilities.md`](docs/03-capabilities.md),
[`docs/design/resource-egress.md`](docs/design/resource-egress.md).

### Registered scripts

Instead of sending source on every call, deploy scripts as files and execute them by
key. Point `scripts_dir` (in `config.json`) at a directory; every `*.js` file under it
is loaded **once at startup**, keyed by its relative path without the extension
(`acme/billing/pricing.js` → `acme/billing/pricing`):

```json
{ "key": "acme/billing/pricing", "context": { "qty": 3, "price": 5 } }
```

A request must carry **exactly one** of `script` / `key` (400 `SCRIPT_XOR_KEY`
otherwise); an unknown key is a 404 `SCRIPT_NOT_FOUND`. Both modes execute through the
identical engine path — same sandbox, same fresh context per request, and `config`
stays per-request either way. Key-mode responses echo the key back in `meta.key`. The
registry is read-only at runtime: changing scripts means redeploying files (image
layer, ConfigMap, mounted volume) and restarting — so N replicas stay trivially
consistent. Design notes: [`docs/design/script-registry.md`](docs/design/script-registry.md).

### ES modules (`import` / `export`)

A handler may be authored as a native **ES module** — `export` its handler and `import`
shared helper modules:

```js
import { quote, withTax } from "acme/pricing";

export default function handler(ctx) {
  return json(withTax(quote(ctx.items, ctx.unit)), null);
}
```

Both `export default function handler` and `export function handler` (named) are accepted.
The mode is auto-detected: a source with a top-level `export` runs as a module, anything
else runs as a classic script (`function handler(ctx) { … }` keeps working unchanged).

**Importable modules** are operator-authored libraries under `modules_dir` (in
`config.json`): every `*.js` / `*.mjs` file is loaded **once at startup**, with a specifier
that is its relative path without the extension (`acme/pricing.mjs` → `acme/pricing`). A
handler `import`s by that specifier. Resolution is a pure in-memory lookup with **no
filesystem access** — a script can `import` only registered modules; an unknown or
traversal specifier (`../`, `/etc/…`) never resolves. Modules run in the same sandbox as
the handler (same memory/timeout/`max_ops` budget) and are read-only at runtime, like the
script registry. Author them with any bundler (`esbuild --bundle --format=esm`) and drop
the output in. Authoring how-to: [`docs/modules.md`](docs/modules.md); design notes:
[`docs/design/injectable-modules.md`](docs/design/injectable-modules.md).

### Operational endpoints

Besides `POST /execute` (and its fan-out sibling `POST /batch`), the server exposes two
unauthenticated read-only endpoints for liveness and scraping:

```
GET /health    -> 200 "ok"
GET /metrics   -> 200 Prometheus text (version 0.0.4)
```

`/metrics` is dependency-free (no client library) and reports per-outcome execution
counters plus live resilience signals — so a dashboard or alert can watch shed load and a
flapping database without parsing logs:

| Metric                                 | Type      | Labels                                                                                                                       | Meaning                                                                         |
| -------------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `runlet_executions_total`               | counter   | `outcome` (`success`, `script_error`, `capability_error`, `timeout`, `memory_limit`, `malformed_response`, `internal_error`) | Executions by terminal outcome.                                                 |
| `runlet_rejections_total`               | counter   | —                                                                                                                            | Requests rejected before execution (bad body, routing, oversized).              |
| `runlet_overload_total`                 | counter   | `scope` (`global`, `partition`)                                                                                              | Requests shed by the bulkhead (Tier 1) / partition cap (Tier 5).                |
| `runlet_db_breaker_trips_total`         | counter   | —                                                                                                                            | Cumulative db circuit-breaker open transitions (Tier 3). The breaker runs in `fabricd` (it owns the driver connections), so this box-side series stays present but reports `0` — scrape `fabricd_db_breaker_trips_total` from `fabricd`'s `metrics_listen` endpoint for the live counter. |
| `runlet_bulkhead_permits_available`     | gauge     | —                                                                                                                            | Free global bulkhead permits right now.                                         |
| `runlet_bulkhead_permits_total`         | gauge     | —                                                                                                                            | Configured global bulkhead capacity.                                            |
| `runlet_execution_duration_seconds`     | histogram | `le`                                                                                                                         | Execution wall-clock latency (`_bucket`/`_sum`/`_count`; executions that ran).  |
| `runlet_capability_op_duration_seconds` | histogram | `capability` (`db`/`mongo`/`http`/`mail`/`s3`/`redis`/`amq`/`auth`), `le`                                                            | Per-capability op latency — which downstream is slow, not just total exec time. |

## JS API

### json(data, error)

The return contract. Every handler must return via `json()`:

```js
function handler(ctx) {
  if (!ctx.name) {
    return json(null, { message: "name is required" });
  }
  return json({ greeting: "hello " + ctx.name }, null);
}
```

### emit(kind, value) — tagged effects

Always available (no config). While `return json(...)` carries *the answer*, `emit(kind, value)`
proposes **structured side outputs** — an audit trail, a stream of findings, or an intent for a
trusted host to perform. They come back on the response as an ordered top-level
`effects: [{ kind, value }]` list — **even if the handler later throws**, so a partial run keeps
everything it emitted ("logic proposes, the host disposes").

```js
function handler(ctx) {
  emit("decided", { tier: "tier-3", reason: "spend > 10k" }); // an audit/decision trail
  for (const m of ctx.mismatches) emit("finding", m);         // itemized findings, kept on a throw
  return json({ ok: true }, null);
}
// → { "data": { "ok": true }, "error": null, "meta": {...},
//     "effects": [ { "kind": "decided", "value": {...} }, { "kind": "finding", "value": {...} }, ... ] }
```

- `kind` is a **required** non-empty string routing tag (≤ 64 chars); `value` is any JSON value,
  passed through verbatim and opaque to the platform.
- A missing, empty, non-string, or over-long `kind` throws and records nothing; the per-execution
  emit count is bounded by `max_ops`.
- A run that never emits carries no `effects` key. See
  [`docs/design/effects-channel.md`](docs/design/effects-channel.md) for the design rationale.

### log.* — diagnostic logging

Always available (no config). The sandbox has **no `console`**; `log.*` is the structured,
leveled diagnostic channel — **diagnostics, not billing; lossy by design.** Each call takes a
Serilog-style message template plus named properties; the entry keeps the template, the properties,
and the rendered message.

```js
function handler(ctx) {
  log.info("charged {user} {amount}", { user: ctx.id, amount: "10.00" }); // → "charged 42 10.00"
  const l = log.with({ requestId: ctx.reqId }); // bound context on every entry from `l`
  if (retry) l.warn("retrying attempt {n}", { n });
  return json({ ok: true }, null);
}
```

- Levels `trace` < `debug` < `info` < `warn` < `error`; a call below the configured floor
  (default `info`) is discarded cheaply — no allocation, no capture.
- `log.with(fields)` derives a logger with bound context merged into every entry (a per-call key
  overrides a bound one).
- **Lossy + bounded:** capped at 256 entries / 256 KB per entry / 1 MB total per execution; an
  oversize entry is truncated. Entries logged before a handler throws are still captured.
- **Routing is platform policy, not the script's choice.** Logs stream to a per-tenant diagnostic
  channel and — only when the trusted gateway requests it — are mirrored inline on the response as
  a top-level `logs` list. A caller cannot force capture.

**`log` vs `emit`:** `emit` is what the run *did* (platform-facing, always-on, part of the
reproducible outputs — billing/audit/intent); `log` is how it *went* (developer-facing,
level-filtered, lossy, outside the reproducible contract). See
[`docs/design/diagnostic-logging.md`](docs/design/diagnostic-logging.md) for the rationale.

### $ / Decimal — exact decimal math

Always available (no config). Backed by the same `rust_decimal` engine that reads
`NUMERIC` columns, so in-script math matches the database exactly. JavaScript has no
operator overloading, so use **methods**, not `+ - * /`:

```js
function handler(ctx) {
  var total = $("19.99").mul(ctx.qty).add("0.01").round(2);
  // methods: add sub mul div neg abs round(places) toCents(places) fromCents(places)
  //          cmp eq lt lte gt gte isZero
  // output:  toString() | toNumber() (lossy) | json() serializes as the exact string
  return json({ total: total }, null); // { "total": "..." }
}
```

`.round()` is half-up. `.toCents()` / `.fromCents()` convert major↔minor units (places
defaults to 2). Holds ~28–29 significant digits. Divide-by-zero and overflow throw.
See [`docs/05-decimal.md`](docs/05-decimal.md).

### http.get / post / put / patch / delete

HTTP client (requires `config.allowed_hosts`):

```js
function handler(ctx) {
  var users = http.get("https://api.example.com/users", { page: 1 });
  // users = { status: 200, data: [...] }

  var created = http.post("https://api.example.com/users", { name: ctx.name });

  // Optional headers (last arg) — cannot override Content-Type
  var auth = http.get("https://api.example.com/me", null, {
    Authorization: "Bearer " + ctx.token,
  });

  return json(users.data, null);
}
```

**SSRF-guarded (script-controlled target).** Only `http`/`https` URLs are reachable — any
other scheme (`file`/`gopher`/`ftp`/`data`/…) is refused up front and on every redirect hop,
so a cross-protocol redirect is never followed. Private/internal addresses are blocked
(including alt-encoded literals like `2130706433`/`0x7f000001`/`127.1` and IPv6-wrapped forms),
and the client pins the classifier-validated address at connect (no DNS-rebinding TOCTOU). The
guard is **framework-enforced for every `ScriptControlled` capability**, not a per-capability
add-on. The in-engine guard is the first line; a **network-layer egress control** (firewalled
netns / egress proxy) is the recommended independent second line at deploy time (see
`docs/security-hardening.md`) — defense in depth, not a replacement.

### io.call(name, action, payload) — logical egress

`io.call` is the **one primitive** for reaching a database, cache, queue, mail relay, or any
service. `name` is a logical resource nickname the request lists in its `config.io` allowlist;
`action` and `payload` are opaque to the box and forwarded verbatim. It returns the parsed JSON
the resource answers, or **throws** a `__runlet`-tagged error (`RESOURCE_NOT_FOUND` for a
nickname absent from the allowlist; a capability code otherwise — see `docs/99-errors.md`).

```js
function handler(ctx) {
  // A database served by the reference broker: the broker maps "orders" to Postgres.
  var users = io.call("orders", "query", {
    sql: "SELECT id, name FROM users WHERE active = $1",
    params: [true],
  });
  // users = { columns: [...], rows: [...], row_count: 1, truncated: false }

  // A cache served box-direct from a co-located loopback service.
  io.call("cache", "set", { key: "u:" + ctx.user_id, value: "1", ttl: 60 });

  return json(users.rows, null);
}
```

A nickname resolves one of two ways, both carrying the **identical** `{action, payload}`
envelope so a service can move between them with no script change:

- **Box-direct** — bound to a co-located loopback endpoint in the box's global `local_resources`
  config; the box POSTs the envelope over plain HTTP, no broker. Loopback-only (boot-guard
  enforced).
- **Broker** — resolved by the `fabricd` egress sidecar, which holds the driver + credentials
  (the box holds none). See [Logical resources](#logical-resources-configio).

Driver-backed capabilities (Postgres, Redis, RabbitMQ/NATS, SMTP, OIDC/IAM, …) are **not shipped**
in the box — compose your own `CapabilityDef`, run the reference broker, or reach a local service
box-direct. The three extension paths + the box-direct shortcut are the whole story in
[`docs/03-capabilities.md`](docs/03-capabilities.md).

### s3.upload_url / s3.download_url / s3.upload_form / s3.sign_url

Presigned-URL generator for direct browser uploads/downloads (requires `config.s3`):

```js
function handler(ctx) {
  // Sign a URL the browser uses to PUT the file straight to the bucket.
  var put = s3.upload_url({ key: "uploads/" + ctx.filename, expires: 300 });
  // put = { url: "https://...&X-Amz-Signature=...", method: "PUT", expires: 300 }

  // Sign a short-lived download link.
  var get = s3.download_url({ key: "uploads/" + ctx.filename });

  // s3.sign_url({ method, key, expires }) is the general form (PUT/GET/HEAD/DELETE).
  return json({ upload: put.url, download: get.url }, null);
}
```

The server **never connects** to the object store — signing is pure AWS SigV4 crypto.
The signed URL goes back to the script, which hands it to the frontend; the browser
does the actual transfer. `expires` is in seconds (clamped to `[1, max_expires]`,
default 15 min, SigV4 max 7 days).

**SSRF-guarded like `http`:** the `endpoint` must use the `http`/`https` scheme
(no `file://`), and its host is checked against the same private/internal-IP blocklist
([`src/ssrf.rs`](src/ssrf.rs)) — `localhost`, `127.0.0.1`, `10.x`, `192.168.x`,
link-local, etc. are **rejected** (one DNS lookup resolves hostnames). So a presigned
URL can only ever target a **publicly reachable** object store, never a local or
internal one. The sandboxed script cannot set `endpoint`; only operator config can.

> ⚠️ Because of the guard, a `MinIO`/S3 instance on `localhost` or a private LAN is
> **blocked** — point `s3` at a public endpoint (AWS S3, Cloudflare R2, or `MinIO`
> exposed on a public address). For local development, set top-level `"debug": true` in
> the server config to relax this (see [Configuration](#configuration)); never in production.

`config.s3` (trusted, caller-supplied in the request — the sandboxed script can never
set the endpoint; unlike the driver-backed capabilities it stays in-engine, so it is
**not** a `fabricd` resource). Works with any SigV4 store — AWS S3, Cloudflare R2,
MinIO, Backblaze B2, DigitalOcean Spaces:

```json
{
  "endpoint": "https://ACCOUNT.r2.cloudflarestorage.com",
  "region": "auto",
  "bucket": "uploads",
  "access_key": "AKID...",
  "secret_key": "SECRET...",
  "path_style": false,
  "expires": 900,
  "max_expires": 604800,
  "max_upload_size": "25mb"
}
```

| Field             | Default    | Description                                                           |
| ----------------- | ---------- | --------------------------------------------------------------------- |
| `endpoint`        | (required) | Public store URL incl. scheme (`https://s3.us-east-1.amazonaws.com`)  |
| `region`          | (required) | SigV4 region scope (`us-east-1`; R2 uses `auto`)                      |
| `bucket`          | (required) | Bucket name                                                           |
| `access_key`      | (required) | Access key id                                                         |
| `secret_key`      | (required) | Secret access key                                                     |
| `path_style`      | `false`    | `true` = `host/bucket/key` (MinIO); `false` = `bucket.host/key` (AWS) |
| `expires`         | `900`      | Default link lifetime in seconds                                      |
| `max_expires`     | `604800`   | Hard cap on link lifetime (SigV4 max, 7 days)                         |
| `max_upload_size` | (unset)    | **`upload_form` only** — max object bytes, human-readable (`"25mb"`)  |
| `allow_delete`    | `false`    | Enable `s3.delete` + presigning `DELETE` URLs (destructive — opt-in)  |

#### s3.upload_form — size-enforced browser uploads

`upload_url` does not cap the body size. `upload_form` returns a **POST policy** whose
`content-length-range` the object store **enforces** — it rejects an upload larger than
`config.s3.max_upload_size`. The cap is **operator-config only**; the script supplies just
the `key` and can never set or raise the size (it cannot read it from `ctx`). This is the
primitive for storage quotas.

```js
function handler(ctx) {
  // max size comes from config.s3.max_upload_size — NOT from ctx.
  var up = s3.upload_form({
    key: "customers/" + ctx.id + "/" + ctx.filename,
    expires: 300,
  });
  // up = { url, fields: { key, "X-Amz-Algorithm", "X-Amz-Credential",
  //                       "X-Amz-Date", "Policy", "X-Amz-Signature" },
  //        max_bytes: 26214400, expires: 300 }
  return json(up, null);
}
```

Frontend (`multipart/form-data`, the `file` field MUST be last):

```js
const form = new FormData();
Object.entries(up.fields).forEach(([k, v]) => form.append(k, v));
form.append("file", file);
await fetch(up.url, { method: "POST", body: form }); // 204 ok · 400 if > max_bytes
```

`config.s3.max_upload_size` is required for `upload_form` (human-readable like
`"25mb"`/`"50gb"`, or bytes). Without it, `upload_form` errors.

#### s3.usage — total bytes/objects under a prefix

```js
function handler(ctx) {
  var u = s3.usage({ prefix: "user-a/" }); // omit prefix → whole bucket
  // u = { prefix: "user-a/", bytes: 5242880, objects: 137 }
  return json(u, null);
}
```

The **only** `s3` op that connects to the store: it signs and sends a `ListObjectsV2`
(`GET /?list-type=2&prefix=…`), pages through `NextContinuationToken`, and sums each
object's `<Size>`. Trusted/operator-config model like `db`/`mail`, but the endpoint host
still goes through the [`ssrf`](src/ssrf.rs) guard ([`resolve_host`](src/s3.rs)); the
list client follows **no redirects**. S3 has no native folder-size API — a prefix is just
a key namespace — so a full scan is the only exact total. Each 1000-key page counts as one
op against `max_ops`, so an oversized prefix fails with the op-limit error instead of
running unbounded; for very large prefixes maintain your own counter (via `db`) and use
`usage` to reconcile. `bytes`/`objects` are returned as JSON numbers (exact below 2⁵³).

#### s3.delete — remove an object (opt-in)

```js
function handler(ctx) {
  var d = s3.delete({ key: "customers/" + ctx.id + "/photo.jpg" });
  // d = { key: "customers/1/photo.jpg", deleted: true }
  return json(d, null);
}
```

Like `usage`, this **connects to the store** (trusted/operator-config, SSRF-guarded host).
It signs and sends a short-lived `DELETE /{bucket}/{key}`. S3 delete is **idempotent** — a
missing key still returns `deleted: true` (HTTP 204). Because deletion is destructive, it
is **gated behind `config.s3.allow_delete`** (default `false`): even with `s3` otherwise
configured, `s3.delete(...)` — and presigning a `DELETE` URL via `s3.sign_url({ method:
"DELETE" })` — throws unless the operator sets `allow_delete: true`. Counts as one op
against `max_ops`.

### $sys — runtime stdlib (crypto, date, env, secrets)

The `$sys` umbrella groups pure, zero-I/O helpers. `$sys.crypto` and `$sys.date` are
**always on** (no config, like `$`); `$sys.env` / `$sys.secrets` populate only from
`config.sys`. Nothing here does network I/O or counts against `max_ops`.

```js
function handler(ctx) {
  // crypto: one-way hashing/signing, IDs, reversible encoders
  $sys.crypto.sha256("hello"); // hex
  $sys.crypto.hmac("sha256", "key", "msg", "base64"); // hex (default) | base64 | base64url
  $sys.crypto.uuid(); // v7 (time-ordered)
  $sys.crypto.base64.encode("hi"); // also .base64url / .hex / .url, each .encode/.decode

  // date: parse (ISO/RFC3339, YYYY-MM-DD, epoch ms → UTC), timedelta math, diff
  var due = $sys.date.parse(ctx.when).add({ days: 3, hours: 12 });
  due.iso(); // RFC 3339 "Z"  ·  due.unix() // epoch seconds  ·  json() serializes as ISO
  $sys.date.parse(b).diff($sys.date.parse(a)); // { total_ms, total_seconds, days, hours, ... }

  return json({ due: due }, null);
}
```

**Secrets are use-not-extract** (the multi-tenant guarantee). With
`config.sys = { "env": { "REGION": "us-east-1" }, "secrets": { "SIGNING_KEY": "sk_live_…" } }`:

```js
$sys.env.REGION; // "us-east-1"  (plain, returnable)
var sig = $sys.crypto.hmac("sha256", $sys.secrets.SIGNING_KEY, body); // ✅ handle → one-way sign
String($sys.secrets.SIGNING_KEY); // "[secret:SIGNING_KEY]"  (never the plaintext)
$sys.crypto.base64.encode($sys.secrets.SIGNING_KEY); // ❌ throws — secrets can't be encoded
```

The plaintext never enters JS — it stays Rust-side and is resolved only by the one-way HMAC
sink, so a script can only ever return the `"[secret:NAME]"` placeholder. There is **no**
output scrubber and **no** reveal escape hatch (both evadable/transmit-to-observable). Use
high-entropy secrets. See [`docs/09-sys.md`](docs/09-sys.md).

## Configuration

> Running it for real? See **[`docs/deployment.md`](docs/deployment.md)** — the production
> hardening checklist (what to set before you point traffic at it, and why).

Optional `config.json` in the working directory. All fields have defaults:

```json
{
  "debug": false,
  "error_debug": false,
  "server": {
    "host": "127.0.0.1",
    "port": 3000
  },
  "engine": {
    "memory_limit": "32mb",
    "max_stack_size": "512kb",
    "timeout_ms": 4000,
    "pool_size": 0,
    "max_script_size": "1mb",
    "max_context_size": 0,
    "max_ops": 1500,
    "max_concurrent_executions": 0
  },
  "scripts_dir": "scripts",
  "fabricd_socket": "/tmp/fabricd.sock"
}
```

| Field                          | Default       | Description                                                                                                                                                                                                                                                                                                                     |
| ------------------------------ | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `debug`                        | `false`       | **Dev only.** Relaxes the SSRF private-IP block for `s3`/`http` so localhost/LAN targets (e.g. MinIO) work. Never enable in production.                                                                                                                                                                                          |
| `error_debug`                  | `false`       | Include `error.debug` (stack traces + raw driver causes) in system-error responses. **Off by default** (secure by default) — the raw cause can carry internal hostnames/driver detail. `meta.trace_id` is always present and the raw cause is always logged server-side, so support can correlate without it.                    |
| `server.host` / `server.port`  | `127.0.0.1` / `3000` | Bind address and port.                                                                                                                                                                                                                                                                                                   |
| `memory_limit`                 | `"32mb"`      | Max JS heap per execution                                                                                                                                                                                                                                                                                                       |
| `max_stack_size`               | `"512kb"`     | Max native call stack (recursion depth)                                                                                                                                                                                                                                                                                         |
| `timeout_ms`                   | `4000`        | Max wall-clock execution time                                                                                                                                                                                                                                                                                                   |
| `pool_size`                    | `0` (auto)    | QuickJS runtime pool size (0 = CPU cores)                                                                                                                                                                                                                                                                                       |
| `max_script_size`              | `"1mb"`       | Max script source size                                                                                                                                                                                                                                                                                                          |
| `max_context_size`             | `0` (auto)    | Max context JSON size. `0` auto-derives `memory_limit / 8`; explicit values are capped at `memory_limit / 4` (boot fails if exceeded).                                                                                                                                                                                          |
| `max_ops`                      | `1500`        | Max HTTP + DB operations per execution                                                                                                                                                                                                                                                                                          |
| `max_output_size`              | `0` (off)     | Max bytes of JSON the handler may return (`0` = bounded only by `memory_limit`). Over the cap fails `422 OUTPUT_TOO_LARGE`. Set in untrusted-script deployments.                                                                                                                                                                |
| `max_concurrent_executions`    | `0` (auto)    | Bulkhead: max in-flight executions. `0` auto-derives `pool_size × 16`. Excess load fast-fails `OVERLOADED` (`503` + `Retry-After`, never `429`). Tune to your DB/PgBouncer connection budget.                                                                                                                                   |
| `max_concurrent_per_partition` | `0` (off)     | Per-partition fairness (per-pod backstop): max concurrent executions per `X-Partition-Key` (or `partition` field). `0` = off. A key over its share fast-fails `PARTITION_OVERLOADED` (`503`, never `429`) even when global capacity remains, so one noisy key can't monopolize a pod. Not a global guarantee — the gateway owns global fairness. |
| `partition_buckets`            | `0` (256)     | Hashed partition buckets (used only when `max_concurrent_per_partition > 0`). More buckets = fewer key collisions.                                                                                                                                                                                                              |
| `allow_wildcard_hosts`         | `false`       | Honor `allowed_hosts: ["*"]` (removes the host allowlist, leaving only the private-IP filter). Off by default: a `*` matches nothing. Never honored while `debug` is on.                                                                                                                                                        |
| `scripts_dir`                  | _(unset)_     | Directory of registered scripts for execute-by-key. Unset = inline `script` only; `key` requests answer `SCRIPT_NOT_FOUND`.                                                                                                                                                                                                     |
| `modules_dir`                  | _(unset)_     | Directory of injectable ES modules for handler `import`. Unset = `import` never resolves. See [ES modules](#es-modules-import--export).                                                                                                                                                                                        |
| `access_token`                 | _(unset)_     | Shared-secret bearer token gating `/execute` (constant-time compared); `/health` and `/metrics` stay open. Required on a non-loopback bind unless `allow_unauthenticated` is set.                                                                                                                                               |
| `allow_unauthenticated`        | `false`       | Explicit opt-out: allow a non-loopback bind with no `access_token` (auth terminated upstream). Without it, an exposed tokenless bind **refuses to start** (fail-closed).                                                                                                                                                        |
| `fabricd_socket`               | _(unset)_     | Path to the `fabricd` egress sidecar's Unix socket. **Required for any broker-resolved `io` resource** — `fabricd` holds the `resources` credential table; the box only forwards `config.io` names. Unset + a broker request ⇒ `503 EGRESS_UNAVAILABLE`.                                  |
| `fabricd_quic`                 | _(unset)_     | Remote `fabricd` over QUIC (alternative to `fabricd_socket`): `{ replicas, server_name, server_cert_pin, auth_token \| auth_token_file }`. The box pins the daemon cert by SHA-256 fingerprint and presents an auth token. See [`docs/design/network-fabric.md`](docs/design/network-fabric.md).                                |
| `local_resources`              | _(none)_      | Box-direct `io` bindings: logical name → `{ url }` co-located **loopback** endpoint the box POSTs the `{action, payload}` envelope to directly (no broker). Loopback/private only — the boot guard refuses a remote binding. A listed `config.io` name bound here resolves box-direct; any other forwards to the broker.        |
| `trusted`                      | _(off)_       | Trusted-identity ("nexus edge") mode: `{ enabled, assert_network_isolation, headers, capability_entitlements, quota }`. Derives tenant/user identity from edge-injected headers; refuses an exposed bind unless isolation is asserted. See [`docs/design/multitenant-trust.md`](docs/design/multitenant-trust.md).              |
| `telemetry`                    | _(off)_       | Tracing/logging: `{ otlp_endpoint, sample_ratio, service_name }`. No `otlp_endpoint` (default) = structured JSON logs only, no OTLP export.                                                                                                                                                                                     |
| `events`                       | _(off)_       | Per-tenant usage + audit event emission: `{ enabled, buffer }` (default `false` / `4096`). Non-blocking, drop-on-full.                                                                                                                                                                                                          |
| `timeout_retryable`            | `true`        | Whether a wall-clock `TIMEOUT` is classified retryable (`true ⇒ 503` retry, `false ⇒ 422` park). The box can't tell a slow dependency from a slow algorithm — flip it `false` for compute-heavy deterministic workloads. Governs **only** `TIMEOUT`; `MEMORY_LIMIT`/op-cap stay non-retryable (`422`) regardless.                |
| `retry_after_seconds`          | `1`           | Seconds advertised in the `Retry-After` header on a retryable `503`/`500` (capacity, quota, dependency outage, retryable timeout). The status says "retry", this bounds the backoff.                                                                                                                                            |

Size fields accept `"8mb"`, `"256kb"`, `"1gb"`, or plain numbers in bytes.

**Context vs. memory.** Parsing a JSON context into JS objects costs ~4× its text size in heap, and a typical transform needs ~6×. So `max_context_size` is tied to `memory_limit`: leave it `0` and it auto-derives `memory_limit / 8` (room to parse _and_ process the input), while any explicit value is hard-capped at `memory_limit / 4` — the point past which a context can't even be parsed. Change `memory_limit` and the context limit follows; to handle larger contexts, raise `memory_limit` rather than lifting the context cap alone.

## Sandbox

Every execution runs in an isolated QuickJS context with:

- Memory limit (configurable)
- Stack size limit (configurable)
- Execution timeout with interrupt handler
- `eval()` and `Proxy` removed from globals
- Fresh context per request (no state leaks)
- HTTP host allowlist per request
- Operation rate limiting per execution
- Input size validation

## Testing

```sh
# Start backing services (PostgreSQL, PgBouncer, CockroachDB, local httpbin, …)
docker compose up -d

# Run the test suite (starts the server itself if one isn't running)
python tests/test_simple.py

# Stop databases
docker compose down
```

## Architecture

```
HTTP request
  -> axum handler (async)
    -> spawn_blocking (off tokio thread pool)
      -> acquire pooled QuickJS runtime
        -> fresh Context per request
          -> inject json() bridge
          -> inject http.* (if allowed_hosts)
          -> inject io.call (if config.io names any resource; gated by the allowlist,
             resolved box-direct or via the broker Egress port)
          -> inject s3.* (if config.s3)
          -> eval user script
          -> remove eval/Proxy
          -> call handler(context)
        <- extract JSON result
      <- release runtime to pool (GC first)
    <- attach meta (sizes, timing, per-capability metrics)
  <- {data, error, meta} response
```

## License

MIT
