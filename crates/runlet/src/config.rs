//! Server configuration loaded from an optional `config.json` file.
//!
//! The HTTP front's own config: bind address, the `/execute` auth gate, and the script/
//! module directories — plus the embedded [`EngineConfig`] sandbox limits owned by
//! `runlet-core`. All fields have sensible defaults; a missing file starts with defaults.
//!
//! Size fields accept human-readable strings: `"8mb"`, `"256kb"`, `"1gb"`,
//! or plain numbers in bytes: `8388608`.

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use runlet_core::config::EngineConfig;
use serde::Deserialize;

use crate::quota::PlanLimit;

/// Default `Retry-After` delay (seconds) attached to a retryable `503`/`500` when no
/// circuit-breaker cool-down applies (the box's breakers live in `fabricd`, so this is the
/// usual value). A short floor: the status already says "retry", the header only bounds the
/// backoff — a generic worker adds its own jitter on top.
const DEFAULT_RETRY_AFTER_SECONDS: u32 = 1;

/// Top-level configuration. `Default` is hand-written (not derived) so the two policy fields that
/// must not default to their type-zero — `timeout_retryable` (default `true`) and
/// `retry_after_seconds` (default [`DEFAULT_RETRY_AFTER_SECONDS`]) — carry the intended value both
/// in Rust (`Config::default()`) and via serde (a missing key falls back to this same `Default`).
/// Every other field keeps its secure/empty zero (`error_debug: false`, `access_token: None`, …).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent operator on/off switches (debug, error_debug, allow_unauthenticated, \
              timeout_retryable), not a state machine — a two-variant enum per flag would obscure \
              the flat JSON config contract"
)]
pub(crate) struct Config {
    /// Local-dev switch. When `true`, the SSRF private-IP block is relaxed so
    /// localhost / LAN targets (e.g. `MinIO`) work for `s3` and `api`. Never enable in
    /// production — it removes the guard against internal/local targets.
    pub(crate) debug: bool,
    /// Include `error.debug` (stack traces + raw driver causes) in responses. Default
    /// `false` (secure by default): the raw cause can carry internal hostnames / driver
    /// detail, so an operator running purely internally opts *in* to the verbosity. The
    /// `trace_id` is always present and the raw cause is always logged server-side, so
    /// support can correlate without leaking detail across the boundary. Kept separate from
    /// `debug` (which only relaxes the SSRF guard) so the two don't entangle.
    pub(crate) error_debug: bool,
    /// Server configuration.
    pub(crate) server: ServerConfig,
    /// JS engine sandbox limits.
    pub(crate) engine: EngineConfig,
    /// Directory of registered scripts (`*.js`), loaded once at startup; a script's
    /// key is its relative path without the extension (`acme/billing/pricing.js` →
    /// `acme/billing/pricing`). Omit to disable execute-by-key (`key` requests then
    /// fail with `SCRIPT_NOT_FOUND`).
    pub(crate) scripts_dir: Option<PathBuf>,
    /// Directory of injectable ES modules (`*.js` / `*.mjs`), loaded once at startup; a
    /// module's specifier is its relative path without the extension (`acme/pricing.mjs`
    /// → `acme/pricing`). A handler `import`s them by that specifier. Omit to disable
    /// `import` (any `import` of a module then fails to resolve).
    pub(crate) modules_dir: Option<PathBuf>,
    /// Shared-secret bearer token gating `/execute`. When set, a request must carry
    /// `Authorization: Bearer <token>` (constant-time compared) or it is rejected `401
    /// UNAUTHORIZED`. `/health` and `/metrics` stay open (probe/scrape paths). This is
    /// defense in depth behind the gateway, not a replacement for it — the `/execute` caller
    /// is fully trusted (it supplies credentials), so an unauthenticated reachable port is a
    /// full compromise. Omit only when auth is genuinely terminated upstream (see
    /// `allow_unauthenticated`).
    #[serde(default)]
    pub(crate) access_token: Option<String>,
    /// Explicit acknowledgement that `/execute` may run without a token on a non-loopback
    /// bind (auth handled by an upstream gateway/mesh). Default `false`: the box **refuses to
    /// start** on a non-loopback address when no `access_token` is set, so a misconfigured
    /// deployment fails closed instead of silently exposing an unauthenticated executor. A
    /// loopback bind never needs this.
    #[serde(default)]
    pub(crate) allow_unauthenticated: bool,
    /// Path to the `fabricd` egress sidecar's Unix-domain socket. **Required** to use any
    /// driver-backed capability (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`): the box links no
    /// driver and holds no credentials — it sends the request's `config.io` logical names to
    /// `fabricd`, which resolves them against its own operator config and performs the I/O. Omit
    /// when the deployment serves only deterministic / `http` / `s3` capabilities; a request that
    /// names a driver resource with no `fabricd_socket` set is rejected `503 EGRESS_UNAVAILABLE`.
    /// See `docs/design/resource-egress.md` step 5.
    #[serde(default)]
    pub(crate) fabricd_socket: Option<String>,
    /// Remote `fabricd` over QUIC — the alternative to `fabricd_socket` for a shared `fabricd`
    /// cluster service on a different host. When set (and `fabricd_socket` is not), driver-backed
    /// capabilities route over QUIC to one of the configured replicas. See
    /// `docs/design/network-fabric.md` (QUIC remote transport).
    #[serde(default)]
    pub(crate) fabricd_quic: Option<FabricdQuic>,
    /// Box-direct local egress bindings (byo-capabilities D8): logical resource name → a co-located
    /// loopback endpoint the box POSTs the `{action, payload}` envelope to **directly**, without a
    /// broker. Operator-only (global config; never per-request, never script-influenced). A name in
    /// `config.io` that is bound here resolves box-direct; any other named name forwards to the
    /// broker. Each target MUST be loopback/private — the boot guard ([`Self::check_local_resources`])
    /// refuses a remote binding (a remote logical target must go through a broker). Empty by default.
    #[serde(default)]
    pub(crate) local_resources: HashMap<String, LocalResource>,
    /// Trusted-identity ("nexus edge") mode — off by default. When enabled the box consumes
    /// trusted identity headers the edge injects (tenant/user/roles/entitlements/suspended/
    /// anonymous), keys fairness + cache + egress + quota off the trusted tenant id, and rejects
    /// anonymous/suspended principals. Requires network isolation (see the boot guard) because the
    /// box then blindly trusts `x-*`. See `docs/design/multitenant-trust.md`.
    #[serde(default)]
    pub(crate) trusted: TrustedConfig,
    /// Distributed-tracing + structured-logging config (the `telemetry` block). Off by default:
    /// with no `otlp_endpoint` the box emits structured JSON logs only (no OTLP export). See
    /// `telemetry.rs` and `docs/design/nexus-upstream-requirements.md` (N6).
    #[serde(default)]
    pub(crate) telemetry: TelemetryConfig,
    /// Per-tenant usage + audit event emission (the `events` block). Off by default. See
    /// `events.rs`.
    #[serde(default)]
    pub(crate) events: EventsConfig,
    /// `POST /batch` fan-out caps (the `batch` block): the maximum item count and the combined-input
    /// / total-response byte bounds. Always present with modest defaults; the endpoint is additive
    /// (a request that never calls `/batch` is unaffected). See `docs/design` (batch-execute-endpoint).
    #[serde(default)]
    pub(crate) batch: BatchConfig,
    /// Whether a wall-clock `TIMEOUT` is classified retryable (default `true`). The box cannot tell
    /// a slow dependency (retrying helps) from a slow algorithm (retrying wastes budget), so this is
    /// an operator knob: `true` projects `TIMEOUT` to a `503` (retry, the retry ladder bounds a
    /// runaway), `false` to a `422` (park). Governs **only** `TIMEOUT`; `MEMORY_LIMIT` and the
    /// op-count cap are deterministic for a given `(script, input)` and stay non-retryable (`422`)
    /// regardless. Flip it `false` for compute-heavy deterministic workloads.
    #[serde(default = "default_timeout_retryable")]
    pub(crate) timeout_retryable: bool,
    /// Seconds advertised in the `Retry-After` header on a retryable `503`/`500` when no
    /// circuit-breaker cool-down applies. Default [`DEFAULT_RETRY_AFTER_SECONDS`].
    #[serde(default = "default_retry_after_seconds")]
    pub(crate) retry_after_seconds: u32,
    /// Operator-global default currency for `$` / `money` construction — the last level of the
    /// three-level cascade (explicit arg → per-request `config.currency` → this). An ISO 4217 code
    /// (e.g. `"USD"`). Omit to leave money construction currency-less: a `$("19.99")` with no
    /// per-request currency then throws a plain-language error asking for one.
    #[serde(default)]
    pub(crate) default_currency: Option<String>,
}

/// serde default for [`Config::timeout_retryable`] — retry a timeout unless the operator opts out.
const fn default_timeout_retryable() -> bool {
    true
}

/// serde default for [`Config::retry_after_seconds`].
const fn default_retry_after_seconds() -> u32 {
    DEFAULT_RETRY_AFTER_SECONDS
}

impl Default for Config {
    fn default() -> Self {
        Self {
            debug: false,
            error_debug: false,
            server: ServerConfig::default(),
            engine: EngineConfig::default(),
            scripts_dir: None,
            modules_dir: None,
            access_token: None,
            allow_unauthenticated: false,
            fabricd_socket: None,
            fabricd_quic: None,
            local_resources: HashMap::new(),
            trusted: TrustedConfig::default(),
            telemetry: TelemetryConfig::default(),
            events: EventsConfig::default(),
            batch: BatchConfig::default(),
            timeout_retryable: default_timeout_retryable(),
            retry_after_seconds: default_retry_after_seconds(),
            default_currency: None,
        }
    }
}

/// `POST /batch` fan-out caps (the `batch` block). Bounds both the request (item count +
/// combined input bytes) and the response (total bytes), so a batch can neither be admitted
/// oversize nor buffer an unbounded response (design D3/D6). Plain byte counts (not the
/// human-readable size strings the engine block uses) — these are HTTP-layer request/response
/// bounds, not engine sandbox limits.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_field_names,
    reason = "the `max_` prefix is the stable JSON config key contract (max_items / max_input_bytes / max_response_bytes / max_shared_bytes)"
)]
pub(crate) struct BatchConfig {
    /// Maximum items in one batch; a larger batch is rejected whole (`400`). Kept modest so a batch's
    /// worst-case pool hold time stays bounded (design D3). Default `25`. The optional `before`/`after`
    /// lifecycle phases do NOT consume item slots (design RQ3) — they are fixed per-batch overhead.
    pub(crate) max_items: usize,
    /// Maximum combined input bytes (Σ script + context over all items); an oversize batch body is
    /// rejected whole (`400`) before any item is admitted. Default `4 MiB`.
    pub(crate) max_input_bytes: usize,
    /// Maximum total response bytes while assembling `results`; an item that would push the running
    /// total past this is truncated to a classified size-limit error envelope rather than buffered
    /// (design D6). Default `8 MiB`.
    pub(crate) max_response_bytes: usize,
    /// Maximum serialized bytes of the immutable shared context built by the `before` phase (design
    /// RQ3/D4). A shared context exceeding this aborts the batch as a `before`-phase barrier failure
    /// (non-200, no items run), never a per-item clamp. Default `4 MiB` (mirrors `max_input_bytes`,
    /// since the shared context is input-shaped data handed read-only to every item).
    pub(crate) max_shared_bytes: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_items: 25,
            max_input_bytes: 4 * 1024 * 1024,
            max_response_bytes: 8 * 1024 * 1024,
            max_shared_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Per-tenant usage + audit event emission (the `events` block). Emits one `usage` event per
/// executed request and one `audit` event per request (allowed / denied-with-reason) to a
/// dedicated stdout stream, non-blocking and fail-open. Off by default (fully inert).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct EventsConfig {
    /// Turn on usage + audit event emission. `false` (default) ⇒ no events, no writer task. Also
    /// gates the diagnostic-log stream sink (logs ride the same pipeline on their own channel).
    pub(crate) enabled: bool,
    /// Bounded `usage`/`audit` channel capacity; beyond it those events are dropped (fail-open) and
    /// `runlet_events_dropped_total` increments. Default `4096`.
    pub(crate) buffer: usize,
    /// Bounded **diagnostic-log** channel capacity — a separate, higher-volume channel isolated from
    /// `usage`/`audit` (D4), so log volume can never drop a billing/audit event. Beyond it, log
    /// events are dropped and `runlet_log_events_dropped_total` increments. Default `8192`.
    pub(crate) log_buffer: usize,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            buffer: 4096,
            log_buffer: 8192,
        }
    }
}

/// Distributed-tracing config (the `telemetry` block). Metrics stay Prometheus PULL; this block
/// governs only trace export + the log/service name. Tracing is enabled iff `otlp_endpoint` is set.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct TelemetryConfig {
    /// OTLP/gRPC collector endpoint (e.g. `http://localhost:4317`). `None` (default) ⇒ tracing
    /// disabled; the box still emits structured JSON logs to stdout. Plaintext by default (a
    /// local/in-pod collector terminates TLS to the backend, not the box).
    pub(crate) otlp_endpoint: Option<String>,
    /// Sampling ratio in `[0.0, 1.0]` for box-started root spans (a parent `traceparent` decision
    /// is always honored). Default `1.0` — sample every self-rooted trace.
    pub(crate) sample_ratio: f64,
    /// `service.name` resource attribute reported to the collector. Default `runlet`.
    pub(crate) service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            sample_ratio: 1.0,
            service_name: "runlet".to_owned(),
        }
    }
}

/// Trusted-identity mode configuration (the `trusted` block).
///
/// `Default` (all off / empty) preserves the pre-change single-principal, caller-asserted-partition
/// behavior: `enabled: false` means no header is trusted and `/execute` behaves exactly as before.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TrustedConfig {
    /// Turn on trusted-header identity mode. When `false`, no `x-*` identity header is read and the
    /// caller-asserted `X-Partition-Key` path stays active (single-tenant behavior).
    pub(crate) enabled: bool,
    /// Operator assertion that this bind is reachable **only** through the edge (enforced out of
    /// band by a k8s `NetworkPolicy`). Required to run trusted-header mode on a non-loopback bind —
    /// the box trusts `x-*` blindly, so an exposed bind without this fails closed (see
    /// [`Config::check_exposure`]). Mirrors `allow_unauthenticated`.
    pub(crate) assert_network_isolation: bool,
    /// The trusted header names (defaults `x-workspace-id`/`x-user-*`/`x-auth-anonymous`/`x-tenant-plan`).
    pub(crate) headers: TrustedHeaders,
    /// Coarse member-capability gate: capability kind (`"db"`, `"mongo"`, …) → the entitlement (or
    /// role) a caller must hold in `x-user-entitlements`/`x-user-roles` to invoke it. A kind absent
    /// from this map is ungated. Empty by default (no member gating).
    pub(crate) capability_entitlements: HashMap<String, String>,
    /// Per-tenant plan-gated quota (section 6). Off by default.
    pub(crate) quota: QuotaConfig,
}

/// The configurable trusted-header names. Defaults match the nexus edge contract; every name is
/// overridable so a drift between the edge and the box is pinned in one place.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct TrustedHeaders {
    /// Tenant / acting-workspace id header (default `x-workspace-id` — the name the nexus identity
    /// sidecar injects; `x-tenant-id` is a legacy fallback inside nexus only and is not read here).
    pub(crate) tenant: String,
    /// User id header, for audit (default `x-user-id`).
    pub(crate) user: String,
    /// Comma-separated roles header (default `x-user-roles`).
    pub(crate) roles: String,
    /// Comma-separated entitlements header (default `x-user-entitlements`).
    pub(crate) entitlements: String,
    /// Suspended-principal flag header (default `x-user-suspended`).
    pub(crate) suspended: String,
    /// Anonymous-caller flag header (default `x-auth-anonymous`).
    pub(crate) anonymous: String,
    /// Tenant plan header, selecting the quota tier (default `x-tenant-plan`).
    pub(crate) plan: String,
    /// Acting-org assurance header (default `x-tenant-scope`). The edge asserts, per request, that
    /// the tenant id is the caller's *authorized acting org* by setting this to `acting`; a
    /// tenant-scoped request whose value is not `acting` is rejected fail-closed (nexus upstream
    /// requirement N5). See `docs/design/multitenant-trust.md`.
    pub(crate) scope: String,
    /// Execution mode header (default `x-runlet-mode`): `live` (default) or `test`/`playground`. A
    /// **test** run is response-mirror-only and never enters the live log stream / billing / audit
    /// (OQ1, Stripe's isolation model). Gateway-asserted, never caller-asserted.
    pub(crate) mode: String,
    /// Diagnostic-capture header (default `x-runlet-capture`): when truthy, the trusted gateway
    /// requests the response-mirror `logs` list for this request (D5, the playground path). Never
    /// caller-asserted.
    pub(crate) capture: String,
    /// Per-request log-level-floor override header (default `x-runlet-log-level`): the gateway MAY
    /// lower the floor for a capture run so the playground gets `debug`/`trace` while production
    /// stays `info`+ (OQ2). Ignored when absent/unparseable.
    pub(crate) log_level: String,
}

impl Default for TrustedHeaders {
    fn default() -> Self {
        Self {
            tenant: "x-workspace-id".to_owned(),
            user: "x-user-id".to_owned(),
            roles: "x-user-roles".to_owned(),
            entitlements: "x-user-entitlements".to_owned(),
            suspended: "x-user-suspended".to_owned(),
            anonymous: "x-auth-anonymous".to_owned(),
            plan: "x-tenant-plan".to_owned(),
            scope: "x-tenant-scope".to_owned(),
            mode: "x-runlet-mode".to_owned(),
            capture: "x-runlet-capture".to_owned(),
            log_level: "x-runlet-log-level".to_owned(),
        }
    }
}

/// Per-tenant plan-gated quota configuration (the `trusted.quota` block).
///
/// Off by default. When `enabled`, every tenant-scoped request is gated: the tenant's plan (from
/// the trusted plan header) selects a [`PlanLimit`], and the tenant's in-flight usage is capped at
/// it. An unknown plan resolves to the most restrictive configured limit, and an **empty** `plans`
/// map denies (fail-closed) — a misconfiguration never grants unbounded usage.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct QuotaConfig {
    /// Consult the quota engine. `false` (default) disables all quota checks.
    pub(crate) enabled: bool,
    /// Plan name → its limit. Empty while `enabled` denies every request (fail-closed).
    pub(crate) plans: HashMap<String, PlanLimit>,
}

/// One box-direct local-egress binding (byo-capabilities D8/D9): a co-located loopback endpoint a
/// logical resource name resolves to. The box POSTs the identical `{action, payload}` envelope a
/// broker would receive, so a name can be moved between box-direct and broker resolution with no
/// script change.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalResource {
    /// The co-located endpoint URL (e.g. `http://localhost:8080`). Loopback/private only — the boot
    /// guard rejects a remote target.
    pub(crate) url: String,
}

/// Remote-`fabricd` QUIC transport settings (the box client side).
///
/// The box pins the daemon's self-signed certificate by fingerprint (no CA / cert manager) and
/// presents an auth token; `fabricd` validates the token and resolves the logical names operator-
/// side. Exactly one of `auth_token` / `auth_token_file` is the credential; omit both only when the
/// daemon's auth provider is disabled.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FabricdQuic {
    /// Replica endpoints to dial (`host:port`); a headless-Service DNS name resolving to many pod
    /// addresses is tried in turn (client-side failover). At least one is required.
    pub(crate) replicas: Vec<String>,
    /// TLS server name presented on the handshake — must match the daemon certificate's name.
    pub(crate) server_name: String,
    /// The daemon certificate's pinned SHA-256 fingerprint, hex-encoded (64 hex chars). The box
    /// trusts exactly this certificate.
    pub(crate) server_cert_pin: String,
    /// A static opaque shared-secret token (mutually exclusive with `auth_token_file`).
    #[serde(default)]
    pub(crate) auth_token: Option<String>,
    /// Path to a k8s projected `ServiceAccount` token file, re-read per session as it rotates
    /// (mutually exclusive with `auth_token`).
    #[serde(default)]
    pub(crate) auth_token_file: Option<PathBuf>,
}

/// HTTP server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct ServerConfig {
    /// Address to bind to.
    pub(crate) host: IpAddr,
    /// Port to listen on.
    pub(crate) port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 3000,
        }
    }
}

impl ServerConfig {
    /// Returns the socket address from host + port.
    pub(crate) const fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

impl Config {
    /// Fail-closed start gate: refuse to bind a **non-loopback** address with no
    /// `access_token` unless the operator explicitly set `allow_unauthenticated` (auth
    /// terminated upstream). A loopback bind is always fine. Keeps a misconfigured
    /// deployment from silently exposing an unauthenticated arbitrary-code executor.
    ///
    /// # Errors
    ///
    /// Returns an error describing the missing gate when the bind is exposed and neither a
    /// token nor the explicit opt-out is present.
    pub(crate) fn check_exposure(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let exposed = !self.server.host.is_loopback();
        if exposed && self.access_token.is_none() && !self.allow_unauthenticated {
            return Err(format!(
                "refusing to start: binding {host} (non-loopback) with no `access_token` and \
                 `allow_unauthenticated` unset. /execute runs caller-supplied code with \
                 caller-supplied credentials, so an unauthenticated reachable port is a full \
                 compromise. Set a token (`access_token` in config, or `RUNLET_ACCESS_TOKEN=<secret>` \
                 in the environment), bind loopback, or opt out with `allow_unauthenticated: true` \
                 (or `RUNLET_ALLOW_UNAUTHENTICATED=1`) if auth is terminated upstream.",
                host = self.server.host,
            )
            .into());
        }
        self.check_trusted_isolation(exposed)?;
        self.check_ssrf_relaxation(exposed)?;
        self.check_local_resources()?;
        Ok(())
    }

    /// Box-direct local-egress boot guard (byo-capabilities D8): every declared binding must target
    /// a loopback/private (co-located) address. A remote target is refused — a remote logical name
    /// must go through a broker, so the box never holds a remote endpoint. Reuses the `http`
    /// capability's classifier ([`runlet_core::check_local_egress_url`]) so one policy governs both
    /// script-controlled `http` targets and operator-declared box-direct bindings.
    ///
    /// # Errors
    ///
    /// Returns an error naming the first binding whose URL is unparseable, non-http(s), or resolves
    /// to any public address.
    #[expect(
        clippy::iter_over_hash_type,
        reason = "boot-guard validation order is irrelevant; every binding is checked and any \
                  failure aborts startup"
    )]
    fn check_local_resources(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        for (name, resource) in &self.local_resources {
            runlet_core::check_local_egress_url(&resource.url).map_err(|err| {
                format!(
                    "refusing to start: box-direct local resource '{name}' -> '{}' is invalid: \
                     {err}. A box-direct binding must be a loopback/private (co-located) http(s) \
                     endpoint; a remote target must go through a broker.",
                    resource.url,
                )
            })?;
        }
        Ok(())
    }

    /// SSRF escape-hatch guard (D5): the private-IP relaxation (`debug`) and the wildcard `*`
    /// host allowlist (`engine.allow_wildcard_hosts`) each collapse the host layer down to the
    /// IP classifier alone, making it the *sole* remaining SSRF defense. Refuse to start with
    /// either active on a non-loopback bind unless network isolation is asserted — mirroring
    /// [`Self::check_trusted_isolation`] and reusing the same operator claim
    /// (`trusted.assert_network_isolation`), so "prod accidentally shipped with the guard
    /// relaxed" is a startup failure rather than a silent exposure. Loopback binds (local dev)
    /// and infra-firewalled deployments that assert isolation are unaffected.
    ///
    /// # Errors
    ///
    /// Returns an error when a relaxation is active, the bind is exposed, and isolation is not
    /// asserted.
    fn check_ssrf_relaxation(&self, exposed: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        let relaxed = self.debug || self.engine.allow_wildcard_hosts;
        if relaxed && exposed && !self.trusted.assert_network_isolation {
            let which = if self.debug {
                "`debug` (relaxes the private-IP SSRF block)"
            } else {
                "`engine.allow_wildcard_hosts` (permits a `*` host allowlist)"
            };
            return Err(format!(
                "refusing to start: {which} is active on {host} (non-loopback) but \
                 `trusted.assert_network_isolation` is unset. That relaxation leaves the IP \
                 classifier as the sole SSRF defense, so the bind must be reachable only through \
                 a trusted edge (enforce with a NetworkPolicy). Bind loopback, drop the \
                 relaxation, or set `trusted.assert_network_isolation: true` once egress is \
                 firewalled at the infra layer.",
                host = self.server.host,
            )
            .into());
        }
        Ok(())
    }

    /// Trusted-mode safety net (D2): trusting `x-*` identity headers rests on the box being
    /// reachable **only** through the edge. Refuse to start in trusted-header mode on a non-loopback
    /// bind unless the operator has asserted network isolation — mirroring the `allow_unauthenticated`
    /// guard, because there is no TLS/JWT check to fall back on once headers are trusted.
    ///
    /// # Errors
    ///
    /// Returns an error when trusted mode is enabled, the bind is exposed, and isolation is not
    /// asserted.
    fn check_trusted_isolation(&self, exposed: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.trusted.enabled && exposed && !self.trusted.assert_network_isolation {
            return Err(format!(
                "refusing to start: trusted-header mode is enabled on {host} (non-loopback) but \
                 `trusted.assert_network_isolation` is unset. The box then trusts `x-*` identity \
                 headers blindly, so it must be reachable only through the edge (enforce with a \
                 NetworkPolicy). Bind loopback, or set `trusted.assert_network_isolation: true` \
                 once the isolation is in place.",
                host = self.server.host,
            )
            .into());
        }
        Ok(())
    }

    /// Loads config from a file path. Returns defaults if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed, or if the
    /// resolved limits violate the parse-headroom invariant (see [`EngineConfig::resolve_limits`]).
    pub(crate) fn load(path: &Path) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut config = if path.exists() {
            let contents = fs::read_to_string(path)?;
            serde_json::from_str::<Self>(&contents)?
        } else {
            Self::default()
        };
        config.apply_env_overrides();
        config.engine.resolve_limits()?;
        Ok(config)
    }

    /// Applies environment-variable overrides for the `/execute` auth gate on top of the
    /// file/defaults. A container image never bakes credentials into a config file, so the two
    /// knobs an operator most needs at `docker run` time are injectable via the environment:
    /// `RUNLET_ACCESS_TOKEN` sets the bearer token (a non-empty value wins over the file), and a
    /// truthy `RUNLET_ALLOW_UNAUTHENTICATED` opts a non-loopback bind out of the token requirement
    /// (auth terminated upstream / a local quickstart). Env wins over the file so the same image
    /// can be pointed either way without editing a mounted config. Reads the env once here and
    /// delegates the (unsafe-free, testable) merge to [`Self::apply_auth_overrides`].
    fn apply_env_overrides(&mut self) {
        let token = env::var("RUNLET_ACCESS_TOKEN").ok();
        let allow = env::var("RUNLET_ALLOW_UNAUTHENTICATED").ok();
        self.apply_auth_overrides(token, allow.as_deref());
    }

    /// Pure merge of the auth-gate env overrides, split out so it is unit-testable without
    /// mutating process env (`std::env::set_var` is `unsafe` in edition 2024, which this crate
    /// forbids). A non-empty `access_token` replaces the configured token; a truthy
    /// `allow_unauthenticated` sets the opt-out (it never *clears* a config-set opt-out).
    fn apply_auth_overrides(
        &mut self,
        access_token: Option<String>,
        allow_unauthenticated: Option<&str>,
    ) {
        if let Some(token) = access_token
            && !token.is_empty()
        {
            self.access_token = Some(token);
        }
        if allow_unauthenticated.is_some_and(is_truthy) {
            self.allow_unauthenticated = true;
        }
    }
}

/// Parses a truthy environment-variable value (case-insensitive, whitespace-trimmed): `1`, `true`,
/// `yes`, or `on`. Everything else — including the empty string and `0`/`false` — is falsey, so a
/// stray or unset variable never silently unlocks the exposure guard.
fn is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    //! The fail-closed exposure gate (`check_exposure`): a non-loopback bind requires either a
    //! token or the explicit `allow_unauthenticated` opt-out.

    use super::{Config, ServerConfig};
    use std::net::{IpAddr, Ipv4Addr};

    /// Builds a config with a chosen bind host, token, and opt-out (everything else default).
    fn exposure_cfg(host: IpAddr, token: Option<&str>, allow_unauth: bool) -> Config {
        Config {
            server: ServerConfig { host, port: 3000 },
            access_token: token.map(str::to_owned),
            allow_unauthenticated: allow_unauth,
            ..Config::default()
        }
    }

    /// A loopback bind never needs a token.
    #[test]
    fn loopback_needs_no_token() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
        assert!(
            cfg.check_exposure().is_ok(),
            "loopback is fine without a token"
        );
    }

    /// A non-loopback bind with no token and no opt-out refuses to start.
    #[test]
    fn exposed_without_token_fails_closed() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
        assert!(
            cfg.check_exposure().is_err(),
            "0.0.0.0 with no token must refuse to start"
        );
    }

    /// A token unlocks an exposed bind.
    #[test]
    fn exposed_with_token_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        assert!(
            cfg.check_exposure().is_ok(),
            "a token unlocks an exposed bind"
        );
    }

    /// The explicit opt-out unlocks an exposed bind (auth terminated upstream).
    #[test]
    fn exposed_with_explicit_optout_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), None, true);
        assert!(
            cfg.check_exposure().is_ok(),
            "allow_unauthenticated unlocks an exposed bind"
        );
    }

    /// The `RUNLET_ACCESS_TOKEN` override sets the token, unlocking an otherwise-exposed bind —
    /// the container path where the secret arrives via env, not a baked config file.
    #[test]
    fn env_token_override_unlocks_exposed_bind() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
        assert!(
            cfg.check_exposure().is_err(),
            "exposed + no token fails first"
        );
        cfg.apply_auth_overrides(Some("from-env".to_owned()), None);
        assert_eq!(cfg.access_token.as_deref(), Some("from-env"));
        assert!(
            cfg.check_exposure().is_ok(),
            "an env-supplied token unlocks the exposed bind"
        );
    }

    /// A truthy `RUNLET_ALLOW_UNAUTHENTICATED` sets the opt-out; an empty value never does (so a
    /// stray/blank env var can't silently unlock the guard).
    #[test]
    fn env_allow_unauthenticated_override() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None, false);
        cfg.apply_auth_overrides(None, Some(""));
        assert!(!cfg.allow_unauthenticated, "blank value stays fail-closed");
        cfg.apply_auth_overrides(None, Some("true"));
        assert!(cfg.allow_unauthenticated, "`true` opts out");
        assert!(cfg.check_exposure().is_ok());
    }

    /// An empty `RUNLET_ACCESS_TOKEN` is ignored (does not blank out a configured token).
    #[test]
    fn env_empty_token_is_ignored() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::UNSPECIFIED), Some("configured"), false);
        cfg.apply_auth_overrides(Some(String::new()), None);
        assert_eq!(
            cfg.access_token.as_deref(),
            Some("configured"),
            "an empty env token must not clobber the configured one"
        );
    }

    /// Truthy parsing is case-insensitive + trimmed; everything else (incl. `0`/`false`) is falsey.
    #[test]
    fn truthy_parsing() {
        for t in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(super::is_truthy(t), "{t:?} should be truthy");
        }
        for f in ["", "0", "false", "no", "off", "maybe"] {
            assert!(!super::is_truthy(f), "{f:?} should be falsey");
        }
    }

    /// Telemetry defaults to disabled (no endpoint), full sampling, service name `runlet`.
    #[test]
    fn telemetry_defaults_disabled() {
        let cfg = Config::default();
        assert!(
            cfg.telemetry.otlp_endpoint.is_none(),
            "no endpoint by default ⇒ tracing off, logs only"
        );
        assert!(
            (cfg.telemetry.sample_ratio - 1.0).abs() < f64::EPSILON,
            "default sample ratio is 1.0"
        );
        assert_eq!(cfg.telemetry.service_name, "runlet");
    }

    /// A `telemetry` block parses its endpoint / ratio / service name; omitted fields default.
    #[test]
    fn telemetry_block_parses() {
        let json = r#"{"telemetry":{"otlp_endpoint":"http://collector:4317","sample_ratio":0.1}}"#;
        let parsed = serde_json::from_str::<Config>(json);
        assert!(parsed.is_ok(), "telemetry block should parse");
        let cfg = parsed.unwrap_or_default();
        assert_eq!(
            cfg.telemetry.otlp_endpoint.as_deref(),
            Some("http://collector:4317")
        );
        assert!((cfg.telemetry.sample_ratio - 0.1).abs() < f64::EPSILON);
        assert_eq!(
            cfg.telemetry.service_name, "runlet",
            "omitted service_name falls back to the default"
        );
    }

    /// Events emission is off by default with a sane buffer; a block parses enabled + buffer.
    #[test]
    fn events_defaults_and_parse() {
        let cfg = Config::default();
        assert!(!cfg.events.enabled, "events off by default (inert)");
        assert_eq!(cfg.events.buffer, 4096);

        let json = r#"{"events":{"enabled":true,"buffer":256}}"#;
        let parsed = serde_json::from_str::<Config>(json);
        assert!(parsed.is_ok(), "events block should parse");
        let parsed_cfg = parsed.unwrap_or_default();
        assert!(parsed_cfg.events.enabled);
        assert_eq!(parsed_cfg.events.buffer, 256);
    }

    /// Batch caps are present with modest defaults; a block parses overrides.
    #[test]
    fn batch_defaults_and_parse() {
        let cfg = Config::default();
        assert_eq!(cfg.batch.max_items, 25, "default item cap");
        assert_eq!(cfg.batch.max_input_bytes, 4 * 1024 * 1024);
        assert_eq!(cfg.batch.max_response_bytes, 8 * 1024 * 1024);
        assert_eq!(
            cfg.batch.max_shared_bytes,
            4 * 1024 * 1024,
            "default shared cap"
        );

        let json = r#"{"batch":{"max_items":100,"max_input_bytes":1024,"max_response_bytes":2048,"max_shared_bytes":512}}"#;
        let parsed = serde_json::from_str::<Config>(json);
        assert!(parsed.is_ok(), "batch block should parse");
        let parsed_cfg = parsed.unwrap_or_default();
        assert_eq!(parsed_cfg.batch.max_items, 100);
        assert_eq!(parsed_cfg.batch.max_input_bytes, 1024);
        assert_eq!(parsed_cfg.batch.max_response_bytes, 2048);
        assert_eq!(parsed_cfg.batch.max_shared_bytes, 512);
    }

    /// `timeout_retryable` defaults to `true` and `retry_after_seconds` to the constant default,
    /// both in Rust and through serde (a missing key falls back to the same `Default`); a block
    /// parses explicit overrides.
    #[test]
    fn retry_policy_defaults_and_parse() {
        let cfg = Config::default();
        assert!(cfg.timeout_retryable, "timeout retries by default");
        assert_eq!(cfg.retry_after_seconds, super::DEFAULT_RETRY_AFTER_SECONDS);

        let empty = serde_json::from_str::<Config>("{}").unwrap_or_default();
        assert!(
            empty.timeout_retryable,
            "a missing key defaults to retry, not the type-zero false"
        );
        assert_eq!(
            empty.retry_after_seconds,
            super::DEFAULT_RETRY_AFTER_SECONDS
        );

        let json = r#"{"timeout_retryable":false,"retry_after_seconds":30}"#;
        let parsed = serde_json::from_str::<Config>(json).unwrap_or_default();
        assert!(!parsed.timeout_retryable, "explicit opt-out parses");
        assert_eq!(parsed.retry_after_seconds, 30);
    }

    /// Builds a config in trusted-header mode with a chosen bind + isolation assertion. A token is
    /// set so the base `access_token` guard passes and only the trusted-isolation guard is exercised.
    fn trusted_cfg(host: IpAddr, assert_isolation: bool) -> Config {
        let mut cfg = exposure_cfg(host, Some("edge-cred"), false);
        cfg.trusted.enabled = true;
        cfg.trusted.assert_network_isolation = assert_isolation;
        cfg
    }

    /// Trusted mode on a loopback bind never needs the isolation assertion.
    #[test]
    fn trusted_loopback_needs_no_isolation() {
        let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), false);
        assert!(
            cfg.check_exposure().is_ok(),
            "loopback trusted mode is fine without asserting isolation"
        );
    }

    /// Trusted mode on an exposed bind without asserted isolation refuses to start.
    #[test]
    fn trusted_exposed_without_isolation_fails_closed() {
        let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), false);
        assert!(
            cfg.check_exposure().is_err(),
            "exposed trusted mode must refuse without asserted isolation"
        );
    }

    /// Asserting isolation unlocks an exposed trusted-mode bind.
    #[test]
    fn trusted_exposed_with_isolation_ok() {
        let cfg = trusted_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), true);
        assert!(
            cfg.check_exposure().is_ok(),
            "asserted isolation unlocks an exposed trusted-mode bind"
        );
    }

    /// Trusted mode disabled leaves an exposed (token-gated) bind unaffected by the isolation guard.
    #[test]
    fn trusted_disabled_ignores_isolation_guard() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        cfg.trusted.enabled = false;
        assert!(
            cfg.check_exposure().is_ok(),
            "isolation guard applies only when trusted mode is enabled"
        );
    }

    /// `debug` (SSRF relaxation) on an exposed bind without asserted isolation refuses to start.
    #[test]
    fn debug_relaxation_exposed_fails_closed() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        cfg.debug = true;
        assert!(
            cfg.check_exposure().is_err(),
            "exposed debug (relaxed SSRF) must refuse without asserted isolation"
        );
    }

    /// A wildcard `*` host allowlist on an exposed bind without asserted isolation refuses to start.
    #[test]
    fn wildcard_hosts_exposed_fails_closed() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        cfg.engine.allow_wildcard_hosts = true;
        assert!(
            cfg.check_exposure().is_err(),
            "exposed wildcard host allowlist must refuse without asserted isolation"
        );
    }

    /// The SSRF relaxation is fine on a loopback bind (local development).
    #[test]
    fn debug_relaxation_loopback_ok() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
        cfg.debug = true;
        assert!(
            cfg.check_exposure().is_ok(),
            "loopback debug relaxation is fine (local testing)"
        );
    }

    /// Asserting network isolation unlocks an exposed bind with the SSRF guard relaxed.
    #[test]
    fn ssrf_relaxation_with_isolation_ok() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        cfg.debug = true;
        cfg.trusted.assert_network_isolation = true;
        assert!(
            cfg.check_exposure().is_ok(),
            "asserted isolation unlocks an exposed relaxed-SSRF bind"
        );
    }

    /// A non-relaxed exposed bind (no debug, no wildcard) is unaffected by the SSRF guard.
    #[test]
    fn no_relaxation_exposed_ok() {
        let cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), Some("tok"), false);
        assert!(
            cfg.check_exposure().is_ok(),
            "the SSRF guard applies only when a relaxation is active"
        );
    }

    /// A loopback-only box-direct binding (D8) passes the boot guard.
    #[test]
    fn local_resource_loopback_ok() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
        drop(cfg.local_resources.insert(
            "pricing".to_owned(),
            super::LocalResource {
                url: "http://127.0.0.1:8080".to_owned(),
            },
        ));
        assert!(
            cfg.check_exposure().is_ok(),
            "a loopback box-direct binding is accepted"
        );
    }

    /// A remote box-direct binding is refused (a remote logical target must go through a broker).
    #[test]
    fn local_resource_remote_fails_closed() {
        let mut cfg = exposure_cfg(IpAddr::V4(Ipv4Addr::LOCALHOST), None, false);
        drop(cfg.local_resources.insert(
            "pricing".to_owned(),
            super::LocalResource {
                url: "http://93.184.216.34:8080".to_owned(),
            },
        ));
        assert!(
            cfg.check_exposure().is_err(),
            "a remote box-direct binding must refuse to start"
        );
    }
}
