# Deployment & production hardening

jsbox is a stateless service: `POST /execute` runs a JS `handler(ctx)` in a
sandboxed QuickJS context and returns `{data, error, meta}`. The box itself links no
network drivers and holds no backend credentials — driver-backed capabilities
(`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) are brokered by the **`fabricd` egress
sidecar** (§5). This guide is the operator's checklist for running it safely under load.
Depth lives in the design notes ([resilience.md](design/resilience.md),
[pooled-capabilities.md](design/pooled-capabilities.md),
[resource-egress.md](design/resource-egress.md), [network-fabric.md](design/network-fabric.md),
[multitenant-trust.md](design/multitenant-trust.md)); this page is the "what to actually
set, and why" synthesis.

> **TL;DR checklist** (each expanded below)
>
> - [ ] `access_token` set (or `allow_unauthenticated: true` if auth is genuinely terminated upstream) — jsbox refuses to start on a non-loopback bind otherwise.
> - [ ] `debug: false` in production config (it relaxes the SSRF guard — local-testing only).
> - [ ] `error_debug` left `false` (default) at an internet-facing edge (keeps stack/raw out of responses).
> - [ ] `allow_wildcard_hosts` left `false` unless a caller genuinely needs `allowed_hosts: ["*"]` (it collapses the host allowlist to the IP filter alone).
> - [ ] `max_output_size` set in an untrusted-script deployment (caps the handler's returned JSON).
> - [ ] `/metrics` and `/health` reachable only from the pod/mesh, never the public internet.
> - [ ] Bulkhead (`max_concurrent_executions`) set to bound DB connections + blocking threads.
> - [ ] `max_statement_timeout_ms` set **in `fabricd`'s config** and a server-side `statement_timeout` role default.
> - [ ] `db_breaker_threshold` > 0 **in `fabricd`'s config** so a dead DB fast-fails instead of pinning threads.
> - [ ] PgBouncer `query_timeout` set if you front Postgres with a transaction-mode pooler.
> - [ ] Global per-tenant fairness lives at the gateway; jsbox's is a per-pod backstop only.
> - [ ] Backend credentials live **only** in `fabricd`'s `resources` config — never in the box's config or the request. Driver capabilities need a running `fabricd` (`fabricd_socket` or `fabricd_quic`).
> - [ ] TLS on every backend connection **from `fabricd`** (`ssl: true` / `tls: true` / `rediss://` / `amqps://` in the resource definitions); secrets via `config.sys`.
> - [ ] Remote (QUIC) `fabricd`: server cert pinned by fingerprint, client auth on (`sa-token` or `static`), `max_connections` sized, NetworkPolicy scoping who can reach it.
> - [ ] Trusted-identity mode only behind the nexus edge: `trusted.assert_network_isolation` asserted **and** enforced out of band with a NetworkPolicy.
> - [ ] k8s: SIGTERM grace ≥ `timeout_ms`, `/health` probes, HPA on CPU or bulkhead headroom.
> - [ ] `task supply-chain` (audit + deny + vet) wired as a CI gate.

## 1. Before you expose it (the non-negotiable gates)

These are the difference between "internal demo" and "safe to point traffic at."

- **Authenticate `/execute` (fail-closed).** The `/execute` caller is trusted — it runs
  arbitrary JS and picks which operator-declared resources to use (`config.io`) — so an
  unauthenticated reachable port is a full compromise (SSRF pivot, mail relay, use of any
  provisioned resource). (Credentials themselves never ride in the request or the box's
  config — they live in the `fabricd` egress sidecar's `resources` config, §5 — but a
  reachable executor is still a pivot into everything the operator provisioned.) jsbox
  **refuses to start** on a non-loopback bind unless you either set `access_token` (a shared
  secret; requests must send `Authorization: Bearer <token>`, constant-time compared) or
  explicitly set `allow_unauthenticated: true` to assert auth is terminated upstream
  (gateway/mesh). `/health` and `/metrics` stay open for probes/scrape. This is defense in
  depth *behind* the gateway, not a replacement for it.
- **`debug: false`.** `debug: true` relaxes the SSRF private-IP block so `api`/`s3` can reach
  localhost/LAN targets — it exists for local testing only. In production it would let a
  script-controlled URL reach your internal network. The default is already `false`; just
  make sure no production `config.json` sets it `true`.
- **`error_debug` at the edge.** `error_debug` (default `false`, secure by default) gates
  whether stack traces and raw driver causes appear in the error envelope's `debug` block.
  Leave it off at any exposed edge so internal detail (hostnames, driver messages) never
  leaves the boundary; set it `true` only on a purely internal service where you want that
  detail inline. The `trace_id` is always present and the raw cause is always logged
  server-side, so support can correlate either way.
- **Scope `/metrics` and `/health`.** Both are unauthenticated, read-only GET endpoints
  (`/metrics` is Prometheus text; see §10). Expose them only to the scrape path / mesh —
  a `NetworkPolicy`, a sidecar, or binding the scrape to the pod IP. Never route them from a
  public ingress.

## 2. Resilience config (map the tiers to knobs)

The full model is [resilience.md](design/resilience.md). The knobs split across the two
processes: the **box** owns the request-side tiers (bulkhead, wall-clock, fairness — its
`engine` block), while **`fabricd`** owns the driver-side tiers (statement-timeout clamp,
connect breaker — its own config, since it holds the driver connections):

| Knob                                                 | Tier | What it does                                                                                                | Production guidance                                                                                                                                                                                                                                            |
| ---------------------------------------------------- | ---- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `max_concurrent_executions` (box)                    | 1    | Bulkhead: caps concurrent executions; excess fast-fails `429 OVERLOADED`.                                   | **Set it explicitly.** The default (auto = cores × 16) is high; size it to your DB connection budget — every concurrent `db` execution holds one connection. A value near your pool/`DEFAULT_POOL_SIZE` keeps a slow DB from exhausting threads + connections. |
| `timeout_ms` (box)                                   | 2    | Wall-clock execution budget; also the per-query client-side DB deadline.                                    | The robust backstop for a hung query. Keep it tight (a few seconds) — it bounds how long a single request can pin a blocking thread.                                                                                                                           |
| `max_statement_timeout_ms` (`fabricd`)               | 0    | Ceiling a `db` resource's `statement_timeout_ms` cannot exceed (clamped at resolve time).                   | Set it (e.g. `1000`–`5000`). Then **also** set a server-side default — see below — because the session `SET` is best-effort through a transaction pooler.                                                                                                      |
| `db_breaker_threshold` / `db_breaker_cooldown_ms` (`fabricd`) | 3 | Per-target circuit breaker: fast-fails a DB that keeps failing to connect.                                | Turn it on (`threshold` 3–5). Measured win under a dead DB: 54× throughput, 281× lower p99 (resilience.md). `0` = off (default).                                                                                                                               |
| `max_concurrent_per_partition` / `partition_buckets` (box) | 5 | Per-`X-Partition-Key` concurrency cap — a noisy key fast-fails `429 PARTITION_OVERLOADED` on its own share. | Optional per-pod backstop. **Global** per-tenant fairness is the gateway's job (see §6). `0` = off.                                                                                                                                                            |

**The Tier 0 server-side default (do not skip).** jsbox issues `statement_timeout` as a
session `SET` at connect. On a direct connection that's a hard guarantee; behind a
**transaction-mode PgBouncer** it is best-effort (the `SET` may bind to a different server
connection than the autocommit query). For a real guarantee set it operator-side:

```sql
ALTER ROLE app_user SET statement_timeout = '5s';
```

or a PgBouncer `connect_query`. See [pooled-capabilities.md](design/pooled-capabilities.md).

**Tier 4 — PgBouncer's own timeouts.** If you front Postgres with PgBouncer, set
`query_timeout` (slightly above your expected `statement_timeout`) and optionally
`query_wait_timeout`. It's an independent layer that catches a runaway query even when the
session `SET` is lost, and below jsbox's wall-clock deadline. There's no jsbox code for this —
it's pooler config.

## 3. Sizing the sandbox

| Knob                                   | Meaning                                                         | Notes                                                                                                                                                                                                                                        |
| -------------------------------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pool_size`                            | Number of pre-warmed runtimes.                                  | `0` = auto (CPU cores). One runtime ≈ one in-flight execution; the bulkhead bounds concurrency above this.                                                                                                                                   |
| `memory_limit`                         | Per-execution heap cap.                                         | Human sizes (`"32mb"`). A fat handler/module eats its own request's budget.                                                                                                                                                                  |
| `max_stack_size`                       | Per-execution stack cap.                                        | Guards runaway recursion.                                                                                                                                                                                                                    |
| `max_script_size` / `max_context_size` | Max bytes for the script and the context payload.               | Validated before execution. `max_context_size` left at `0` auto-derives to `memory_limit / 8`; an explicit value is **capped at `memory_limit / 4`** (the ~4× JSON-parse heap cost), so a max-size context always parses instead of OOM-ing. |
| `max_ops`                              | Cap on total external operations (db/api/mail/…) per execution. | Bounds a handler's downstream fan-out.                                                                                                                                                                                                       |
| `max_output_size`                      | Max bytes of JSON the handler may return.                       | `0` = off (bounded by `memory_limit`). Set it for untrusted scripts so one handler can't return a `memory_limit`-sized blob; over-cap fails `OUTPUT_TOO_LARGE` (422).                                                                         |

Sizes accept human-readable byte strings (`"32mb"`, `"1mb"`). The body limit is derived from
`max_script_size + max_context_size`.

## 4. TLS to backends (connections originate from `fabricd`)

The driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) connect to
**operator-supplied** hosts, so they are trusted and not SSRF-guarded — but the connection
originates from **`fabricd`**, not the box: the box only forwards logical resource names over
its `fabricd` session (§5). Encrypt the backend hop in transit in the **resource
definitions** in `fabricd`'s config:

- `db`: `"ssl": true` (e.g. AWS RDS/Aurora, managed Postgres).
- `mongo`: `"tls": true` (+ optional `"ca_cert"` PEM for a private CA).
- `redis`: `rediss://…` (e.g. ElastiCache in-transit encryption).
- `amq`: `"tls": true` for `amqps://` (+ optional `"ca_cert"`).
- `mail`: `"tls": "starttls"` (or `"wrapper"` for implicit SMTPS).

All of them reuse `fabricd`'s single process-wide `aws-lc-rs` rustls provider — no second
crypto stack. For an internal CA / self-signed cert, the platform trust store applies **in
`fabricd`'s image** — mount your CA bundle into *that* image's trust path (or use the
per-resource `ca_cert` where supported).

The box↔`fabricd` hop itself is either a local Unix-domain socket (same host — no TLS
needed, gated by filesystem permissions) or QUIC with TLS 1.3, a pinned server certificate,
and client auth — see §5.

(`api`/`s3` are the exception: they stay **in-box** — SSRF-guarded HTTP / pure SigV4
presigning — so their egress originates from the box. See
[resilience.md](design/resilience.md) on the two trust models.)

**The `api` host allowlist.** A request's `allowed_hosts` names the hosts the `api` client may
reach; the SSRF guard additionally blocks any host that resolves to a private/internal IP
(IPv4 **and** IPv6 — loopback, RFC1918, link-local incl. cloud-metadata, ULA, and private v4
smuggled via 6to4/NAT64). The wildcard `allowed_hosts: ["*"]` collapses the allowlist to that
IP filter alone, so it is **ignored unless** `allow_wildcard_hosts: true` is set (and never
honored in `debug` mode). Prefer an explicit host list; reach for the wildcard only when a
trusted caller genuinely needs open egress.

## 5. Running `fabricd` (the credential / egress broker)

`fabricd` owns two things the box deliberately does not: the operator `resources`
credential table (logical name → driver kind + endpoint + credentials, loaded from
`FABRICD_CONFIG`, default `fabricd.json`) and every network driver. A request enables a
driver capability by naming a resource in `config.io` (e.g. `"io": {"db": ["orders-db"]}`);
the box forwards only the *names*, and `fabricd` resolves them — credentials never reach
the box. Treat `fabricd.json` as a secret file (it replaces every per-request credential
that used to exist). Full rationale: [resource-egress.md](design/resource-egress.md);
QUIC transport details: [network-fabric.md](design/network-fabric.md).

**No `fabricd`, no drivers — by design.** A deployment serving only deterministic /
`api` / `s3` requests needs no sidecar at all. If a request names a driver resource and
neither `fabricd_socket` nor `fabricd_quic` is configured, it is rejected
`503 EGRESS_UNAVAILABLE`; a name the operator never bound (or bound to another tenant)
is a `400 RESOURCE_NOT_FOUND`.

### As a local sidecar (UDS — the zero-config default)

Run `fabricd` next to the box (same pod/host). It binds a Unix-domain socket
(`FABRICD_SOCKET` env or `socket` in its config; default `/tmp/fabricd.sock`), and the
box points `fabricd_socket` at that path. The socket is gated by **filesystem
permissions** — no token rides the UDS hop — so scope the socket file to the two
processes (in k8s: an `emptyDir` volume shared by the two containers of the pod).
`fabricd` also owns the driver-side resilience knobs (§2): `max_statement_timeout_ms`,
the Tier 0 ceiling clamping any `db` resource's `statement_timeout_ms`, and
`db_breaker_threshold` / `db_breaker_cooldown_ms`, the Tier 3 per-target connect
breaker (daemon-global, so trip state accumulates across sessions). Set `metrics_listen`
(e.g. `127.0.0.1:9090`) to scrape the daemon's own counters —
`fabricd_db_breaker_trips_total`, `fabricd_auth_failures_total` — as plaintext Prometheus
text; scope it like the box's `/metrics` (pod-local / mesh, never a public ingress).

### As a shared network service (QUIC)

Many box pods can share one replicated `fabricd` `Deployment` instead of a per-pod
sidecar. Enable the `quic` block in `fabricd`'s config and `fabricd_quic` in the box's
(one daemon can serve UDS and QUIC at once). Three independent security layers apply —
configure all of them:

- **Encryption / anti-MITM: a pinned self-signed cert.** `fabricd` presents
  `server_cert`/`server_key` (one static self-signed PEM — no CA, no cert-manager); each
  box pins it by SHA-256 fingerprint (`fabricd_quic.server_cert_pin`, 64 hex chars) and
  must send a matching `server_name`. Rotating the cert means updating the pin on every
  box.
- **Client auth (`quic.auth.mode`).** `none` is only safe on a strictly isolated network.
  `static` is an opaque shared secret (`static_token`, constant-time compared;
  `previous_token` allows zero-downtime rotation). `sa-token` — the production primary —
  verifies a k8s projected `ServiceAccount` token offline against the cluster JWKS
  (`audience` + `issuer` required; prefer an explicit in-cluster `jwks_url` + the mounted
  `ca_cert`; fail-closed until the first JWKS fetch). On the box, set exactly one of
  `fabricd_quic.auth_token` (static) or `auth_token_file` (the projected token path,
  re-read per session as the kubelet rotates it).
- **Reachability: a NetworkPolicy** restricting which namespaces/pods may reach the
  `fabricd` Service at all.

Operational knobs: `quic.listen` (`host:port` UDP bind), `quic.max_connections`
(default 1024) caps concurrent connections, and per-connection stream concurrency is
capped by the transport — one misbehaving box can't starve the broker. On the box,
`fabricd_quic.replicas` lists endpoints to dial (a headless-Service DNS name works;
replicas are tried in turn for client-side failover). `fabricd` replicas are stateless —
the `resources` table is read-only config, so scale them like any Deployment.

## 6. Multi-tenant fairness under k8s

The per-partition cap (Tier 5) is a **per-pod** control: under N replicas the effective ceiling
is per-pod × N and drifts with the HPA. **Global** per-tenant fairness belongs at the gateway —
the one component with the fleet-wide view (rate limiting, often Redis-backed). The reference
split: **gateway = global per-key policy** (reject over-quota before fan-out); **jsbox = per-pod
bulkhead + per-pod partition backstop** for sticky-routing / hot-key / gateway-gap cases. Pass
the key via the `X-Partition-Key` header (it wins over a `partition` body field; both are
caller-set, never script-set) and it's echoed back in `meta.partition`. In trusted-identity
mode (§7) the caller-asserted key is **ignored** — fairness is keyed off the trusted tenant id.

## 7. Trusted-identity mode (multi-tenant behind the nexus edge)

Off by default. Enable the `trusted` block only when the box runs **behind the nexus
edge** — an upstream that authenticates callers and injects identity headers the box then
trusts blindly. Full model: [multitenant-trust.md](design/multitenant-trust.md).

- **The boot guard (do not fight it).** With `trusted.enabled: true` on a non-loopback
  bind, the box **refuses to start** unless `trusted.assert_network_isolation: true`. That
  flag is your assertion — enforced out of band with a k8s `NetworkPolicy` — that only the
  edge can reach the bind. There is no TLS/JWT fallback once headers are trusted, so
  isolation is the whole security story.
- **Headers.** Defaults: `x-workspace-id`, `x-user-id`, `x-user-roles`,
  `x-user-entitlements`, `x-user-suspended`, `x-auth-anonymous`, `x-tenant-plan`,
  `x-tenant-scope`. Every name is overridable under `trusted.headers` so edge↔box drift is
  pinned in one place. The edge must set the scope header to `acting` per request
  (acting-org assurance, N5) — a tenant-scoped request without it is rejected fail-closed.
- **What flips on.** Anonymous and suspended principals are rejected; the caller-asserted
  `X-Partition-Key` is dropped and Tier 5 fairness + the bytecode-cache namespace key off
  the trusted tenant id; the tenant id is forwarded in the `fabricd` session handshake, so
  resource resolution is **tenant-scoped** (give each binding in `fabricd`'s `resources` a
  `tenant` — a name bound to another tenant resolves as `RESOURCE_NOT_FOUND`, so existence
  never leaks across workspaces).
- **Authorization + quota (optional).** `trusted.capability_entitlements` maps a
  capability kind (`"db"`, `"mongo"`, …) to the entitlement/role a member must hold; an
  unlisted kind is ungated. `trusted.quota` gates per-tenant in-flight usage by plan (from
  the plan header): an unknown plan gets the most restrictive configured limit, and an
  **empty** `plans` map while enabled denies everything — fail-closed, never unbounded.
- Identity rides **spans, logs, and events** as attributes — never metric labels (§10).

## 8. Secrets

`$sys.secrets` values are **opaque handles** inside JS — the plaintext never enters the sandbox;
a script can only ever return the `"[secret:NAME]"` placeholder, never the value (see
[docs/09-sys.md](09-sys.md)). Supply them in `config.sys`. The request `config` no longer carries
driver credentials (they live in `fabricd`'s `resources` config, §5 — keep *that* file secret),
but `config.sys.secrets` still does, so terminate TLS in front of jsbox and
don't log request bodies.

**Mail relay abuse (untrusted scripts).** A handler chooses its own `to`/subject/body against the
operator's SMTP relay, so for untrusted scripts constrain it in the operator's `mail` resource
(in `fabricd`'s config): set
`allowed_recipient_domains` (a recipient whose domain is off-list is rejected before send) and
`max_sends` (per-execution cap on `mail.send`, on top of `max_recipients` per message). Together
they keep a handler from turning the relay into an open spam cannon.

## 9. Kubernetes specifics

- **Graceful shutdown.** jsbox handles `SIGTERM`/Ctrl-C and drains in-flight requests
  (`axum::serve` with graceful shutdown). Set `terminationGracePeriodSeconds` **≥ `timeout_ms`**
  so an in-flight execution can finish before the kill.
- **Probes.** Liveness and readiness → `GET /health` (returns `200 "ok"`). It's cheap and has no
  dependencies, so it reflects "the process is up," not backend health (by design — backend
  health is per-request and surfaced as retryable capability errors).
- **Autoscaling.** Scale on CPU, or on the bulkhead headroom gauge
  `runlet_bulkhead_permits_available` (scale up as it trends toward zero). A rising
  `runlet_overload_total` rate means you're shedding — add replicas or raise the bulkhead.
- **Image.** The release image is multi-stage → distroless/static, ~18 MB. It runs fine as
  non-root with a read-only root filesystem: the script/module registries load **once at
  startup** and nothing is written at runtime. Mount `scripts_dir` / `modules_dir` read-only
  (image layer, ConfigMap, or volume).
- **Replicas are trivially consistent.** Stateless + registries-at-startup means N replicas
  behave identically; "deploy a new script/module" = roll the image/ConfigMap and restart.

## 10. Observability

Scrape `GET /metrics` (Prometheus text, no client library). The series and suggested alerts:

| Metric                                             | Alert on                                                                             |
| -------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `runlet_executions_total{outcome}`                  | A rising `internal_error` / `timeout` / `capability_error` rate.                     |
| `runlet_overload_total{scope}`                      | Sustained `global` shedding (under-provisioned) or `partition` shedding (a hot key). |
| `runlet_db_breaker_trips_total`                     | Box-side series only — reports `0` since the breaker runs in `fabricd`. Scrape `fabricd`'s `metrics_listen` endpoint instead: any increase in `fabricd_db_breaker_trips_total` = a database is flapping/down; a spike in `fabricd_auth_failures_total` = someone is probing the QUIC listener. |
| `runlet_bulkhead_permits_available` / `_total`      | Available trending toward 0 = at capacity (scale).                                   |
| `runlet_execution_duration_seconds`                 | SLO latency objectives via `histogram_quantile` (p95/p99).                           |
| `runlet_capability_op_duration_seconds{capability}` | Which downstream (db/api/…) is slow, not just total.                                 |

Every response also carries `meta.trace_id`, logged server-side with the raw cause — grep one
ID across the mesh for support.

**Three signals, hybrid transport.** Metrics stay **Prometheus PULL** (`/metrics`, above) — the
low-cardinality, always-scrapable pillar. Logs are **structured JSON to stdout** (the collector
tails them, so a collector outage never loses logs). Traces **PUSH via OTLP** to a collector.
Identity (tenant/user/plan) rides **traces and logs** as attributes — **never** metric labels (the
cardinality rule: identity would explode Prometheus series at multi-tenant scale).

**Enable tracing** with a `telemetry` config block:

```json
"telemetry": {
  "otlp_endpoint": "http://otel-collector:4317",
  "sample_ratio": 0.1,
  "service_name": "runlet"
}
```

- `otlp_endpoint` — OTLP/gRPC collector address. **Omit to disable tracing** (logs still emit).
  Plaintext by default (a local/in-pod collector terminates TLS to the backend, not the box) — so
  no second crypto stack is linked. Point it at a sidecar/daemonset collector.
- `sample_ratio` — fraction of **box-started** root traces to sample (`0.0`–`1.0`). A propagated
  edge `traceparent` decision is always honored; the collector does tail-sampling for errors/slow.
- Export is non-blocking (batch processor, drop-on-full) and fail-open: an unreachable collector
  never fails startup or a request. On graceful shutdown buffered spans are flushed.
- **Edge propagation (N6):** for one trace across edge → box → `fabricd`, the nexus edge must inject
  a W3C `traceparent`; the box continues it. Until then, box-rooted traces still work. See
  `docs/design/nexus-upstream-requirements.md` (N6).

**Per-tenant usage + audit events** (billing/quota-tuning + the compliance trail). Enable with an
`events` block:

```json
"events": { "enabled": true, "buffer": 4096 }
```

- One **unified, versioned event** per request to a dedicated **stdout JSON stream** (distinct from
  the app logs — route on the envelope: `{v, event_id, ts, tenant, user, plan, trace_id, type}`):
  a `usage` event per executed request (outcome, exec time, input bytes, per-capability op counts),
  and an `audit` event per request — `allowed`, or `denied` with a reason code (anonymous /
  suspended / tenant-less / acting-scope / entitlement / quota / oversized / egress / overload).
- Identity (tenant/user/plan) rides **events**, never metric labels (cardinality). Events are
  **unsampled** (every request) — unlike traces.
- Emission is **non-blocking + fail-open**: events go to a bounded channel (`buffer`) drained by a
  writer task; a full buffer **drops** events (the request never blocks) and increments
  `runlet_events_dropped_total` (the backpressure gauge on `/metrics` — watch it and raise `buffer`
  or scale if it climbs). Off by default; fully inert when disabled.
- **`event_id`** is a per-event dedup key: today the stream is observability-grade (lossy under
  pressure); the schema + key are designed so a **durable, billing-grade outbox** can be added later
  (a new sink impl) without changing what the box emits.

## 11. Supply chain

`task supply-chain` runs cargo-audit (advisories) + cargo-deny (licenses/bans/sources) +
cargo-vet (every dependency audited or exempted). It runs as a CI gate on every PR/push
(`ci.yml`, `audit + deny + vet` job), so a new or bumped dependency that isn't vetted fails
the build. The cargo-vet version is pinned in lockstep between `task setup` and `ci.yml`
(the `imports.lock` format is version-sensitive) — bump both together. Releases are CI-only
(`.github/workflows/release.yml`, manual `workflow_dispatch`) — don't hand-edit versions.
