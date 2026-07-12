//! Composable capability registry — the extension model for the logic host.
//!
//! A capability is a first-class value ([`CapabilityDef`]): a name, its JS wrapper source, an
//! editor `.d.ts` fragment, a mandatory [`Trust`] declaration, and an optional locally-bound
//! egress backend. A [`LogicHost`](crate::host::LogicHost) is composed from a set of defs plus
//! an optional fallback egress; the mux ([`CapabilityRegistry::dispatch`]) routes each capability
//! call to the backend bound for its name (or a fallback) and enforces the sandbox invariants
//! centrally: the SSRF guard for [`Trust::ScriptControlled`] targets, and **fail-closed** denial
//! (D9) when its own enforcement cannot be evaluated (a policy hook errors or panics).
//!
//! The in-engine capabilities (`http`/`s3`) do **not** route through this mux — they carry their
//! own in-engine code and are the enumerated bypass surface (see `docs/design/composable-core.md`).

use std::collections::HashMap;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use crate::egress::Egress;
use crate::errors::{self, DynamicFault, ErrorOwner};
use crate::ssrf::block_private_ip;

/// A connection target a [`Trust::ScriptControlled`] call wants to reach, pulled from the
/// (otherwise opaque) call payload so the framework can apply the SSRF guard the backend
/// cannot.
#[derive(Debug, Clone)]
pub struct Target {
    /// Hostname or IP literal the call targets.
    pub host: String,
    /// Port the call targets (for the resolve-and-classify check).
    pub port: u16,
}

/// Pulls the outbound [`Target`] out of a call payload for a script-controlled capability.
///
/// `Ok(None)` = the action carries no outbound target (allowed). `Err` = the payload could not
/// be understood, so the mux denies the call fail-closed rather than dispatching it unchecked.
pub type TargetExtractor = dyn Fn(&str) -> Result<Option<Target>, String> + Send + Sync;

/// The SSRF policy the framework enforces for a [`Trust::ScriptControlled`] capability.
///
/// Before a call reaches its backend the mux applies a host allowlist plus a payload target
/// extractor — the same allowlist + private/internal-IP block the in-engine `http` capability
/// applies, to an egress-routed capability it cannot see inside.
#[derive(Clone)]
pub struct SsrfPolicy {
    /// Allowed hosts; empty = any public host (still IP-blocked). The wildcard opt-in lives with
    /// the in-engine `http` capability and is intentionally not honored here.
    allowed_hosts: Arc<[String]>,
    /// Extracts the `(host, port)` a call targets from its payload.
    extract_target: Arc<TargetExtractor>,
}

impl SsrfPolicy {
    /// Builds a policy from an allowlist and a payload target extractor.
    #[must_use]
    pub fn new(allowed_hosts: Arc<[String]>, extract_target: Arc<TargetExtractor>) -> Self {
        Self {
            allowed_hosts,
            extract_target,
        }
    }

    /// Applies the guard to one call's payload. Fail-closed: an extractor error denies. `Ok(())`
    /// means the call may proceed (either no outbound target, or an allowed public one).
    fn enforce(&self, payload: &str, allow_private: bool) -> Result<(), String> {
        let Some(target) = (self.extract_target)(payload)? else {
            return Ok(());
        };
        if !self.allowed_hosts.is_empty() {
            let host_lower = target.host.to_lowercase();
            let allowed = self
                .allowed_hosts
                .iter()
                .any(|host| host.to_lowercase() == host_lower);
            if !allowed {
                return Err(format!("host '{}' is not in the allowlist", target.host));
            }
        }
        block_private_ip(&target.host, target.port, allow_private)
    }
}

impl fmt::Debug for SsrfPolicy {
    #[expect(
        clippy::renamed_function_params,
        reason = "descriptive name over the trait's terse `f`, matching the crate's min-ident lint"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SsrfPolicy")
            .field("allowed_hosts", &self.allowed_hosts)
            .finish_non_exhaustive()
    }
}

/// The trust model a capability declares for its connection targets — mandatory on every
/// [`CapabilityDef`] so "accidentally built an SSRF hole" is a type error, not a CVE (D4).
#[derive(Debug, Clone)]
pub enum Trust {
    /// Targets come from operator config (the `db`/`mail` model): the backend connects to
    /// whatever host the operator named, so no SSRF restriction is applied.
    OperatorSupplied,
    /// Targets come from script input: the framework applies the [`SsrfPolicy`] before the call
    /// reaches the backend.
    ScriptControlled(SsrfPolicy),
}

/// One composable capability: a name, its JS wrapper, its editor type fragment, a trust
/// declaration, and an optional locally-bound backend.
///
/// Register defs on [`LogicHost::builder`](crate::host::LogicHost::builder). A def with a bound
/// backend serves its own calls in-process; a def without one routes to the request/registry
/// fallback egress (e.g. a broker).
#[derive(Clone)]
pub struct CapabilityDef {
    /// Unique capability name — the JS global, the mux routing key, and the metric key.
    name: Arc<str>,
    /// JS wrapper source, `eval`'d to expose the capability's global (routes through `io.call`).
    js_wrapper: &'static str,
    /// Editor `.d.ts` fragment concatenated into the generated `container/types.d.ts`.
    types: &'static str,
    /// Declared trust model (mandatory) — governs the SSRF guard.
    trust: Trust,
    /// Optional locally-bound backend; `None` routes calls to the request/registry fallback.
    backend: Option<Arc<dyn Egress>>,
}

impl CapabilityDef {
    /// Builds a def with no locally-bound backend (routes to the fallback egress).
    #[must_use]
    pub fn new<N: Into<Arc<str>>>(
        name: N,
        js_wrapper: &'static str,
        types: &'static str,
        trust: Trust,
    ) -> Self {
        Self {
            name: name.into(),
            js_wrapper,
            types,
            trust,
            backend: None,
        }
    }

    /// Binds an in-process backend for this capability (e.g. a `fabric-backends` `*Backend`).
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn Egress>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// The capability name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The JS wrapper source.
    #[must_use]
    pub const fn js_wrapper(&self) -> &'static str {
        self.js_wrapper
    }

    /// The editor `.d.ts` fragment.
    #[must_use]
    pub const fn types(&self) -> &'static str {
        self.types
    }
}

impl fmt::Debug for CapabilityDef {
    #[expect(
        clippy::renamed_function_params,
        reason = "descriptive name over the trait's terse `f`, matching the crate's min-ident lint"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityDef")
            .field("name", &self.name)
            .field("trust", &self.trust)
            .field("backend", &self.backend.as_ref().map(|_egress| "<backend>"))
            .finish_non_exhaustive()
    }
}

/// A per-name routing entry the mux consults at call time.
struct Route {
    /// Locally-bound backend for this name; `None` routes to a fallback egress.
    backend: Option<Arc<dyn Egress>>,
    /// Trust model governing the SSRF guard for this name.
    trust: Trust,
}

/// Building a [`CapabilityRegistry`] failed.
///
/// Carries [`fmt::Display`]/[`fmt::Debug`] but does not implement [`std::error::Error`] (the
/// crate's `missing_trait_methods` lint forbids the partial impl the trait's unstable defaults
/// require); match on it or wrap it at the call site.
#[derive(Debug)]
pub enum RegistryError {
    /// Two defs shared one capability name.
    DuplicateName(String),
}

impl fmt::Display for RegistryError {
    #[expect(
        clippy::renamed_function_params,
        reason = "descriptive name over the trait's terse `f`, matching the crate's min-ident lint"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName(name) => {
                write!(formatter, "duplicate capability name `{name}`")
            }
        }
    }
}

/// One capability call routed through the mux — bundles the FFI arguments plus the enforcement
/// context so [`CapabilityRegistry::dispatch`] stays within the argument-count lint.
pub(crate) struct MuxCall<'a> {
    /// Capability name (the routing key).
    pub(crate) name: &'a str,
    /// Action verb (opaque to the mux; forwarded to the backend).
    pub(crate) action: &'a str,
    /// JSON payload (opaque to the mux; the SSRF extractor may read it).
    pub(crate) payload: &'a str,
    /// Debug SSRF relaxation (server `debug` mode).
    pub(crate) allow_private: bool,
    /// Per-request fallback egress (the broker), consulted after a local backend.
    pub(crate) fallback: Option<&'a Arc<dyn Egress>>,
}

/// The composed capability set for a host: the ordered defs (for wrapper injection + type
/// generation) and the routing table the mux consults at call time. Cheap to [`Clone`]
/// (all state is `Arc`-backed).
#[derive(Clone, Default)]
pub struct CapabilityRegistry {
    /// Registered defs in registration order — drives wrapper injection + type generation.
    defs: Arc<[CapabilityDef]>,
    /// Per-name routing entries (backend + trust) consulted by the mux.
    routes: Arc<HashMap<Box<str>, Route>>,
    /// Fallback egress for names without a local backend (a custom box wires it on the builder;
    /// the stock server supplies a per-request fallback instead — see [`Self::dispatch`]).
    fallback: Option<Arc<dyn Egress>>,
}

impl CapabilityRegistry {
    /// Builds a registry from an ordered def list and an optional fallback egress.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::DuplicateName`] if two defs share a name (D1: the capability set
    /// is validated at host construction, before any request is served).
    pub fn build(
        defs: Vec<CapabilityDef>,
        fallback: Option<Arc<dyn Egress>>,
    ) -> Result<Self, RegistryError> {
        let mut routes: HashMap<Box<str>, Route> = HashMap::with_capacity(defs.len());
        for def in &defs {
            let key: Box<str> = Box::from(def.name());
            if routes.contains_key(&key) {
                return Err(RegistryError::DuplicateName(def.name().to_owned()));
            }
            let _prev = routes.insert(
                key,
                Route {
                    backend: def.backend.clone(),
                    trust: def.trust.clone(),
                },
            );
        }
        Ok(Self {
            defs: Arc::from(defs),
            routes: Arc::new(routes),
            fallback,
        })
    }

    /// The registered defs, in registration order.
    #[must_use]
    pub fn defs(&self) -> &[CapabilityDef] {
        &self.defs
    }

    /// Whether the mux has anything to route — any registered def or a wired fallback. When
    /// `false` the `io` global (and every capability wrapper) is withheld: there is no I/O to do.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.defs.is_empty() || self.fallback.is_some()
    }

    /// Routes one capability call through the mux and returns the FFI JSON — the backend's
    /// success JSON verbatim, or a `__runlet` error tag.
    ///
    /// The per-request fallback ([`MuxCall::fallback`], the stock server's broker) is consulted
    /// after a local backend and before the builder-wired [`Self::fallback`]. **Fail-closed**
    /// (D9): a panic in routing or SSRF-policy evaluation denies the call rather than falling
    /// through to the I/O.
    #[must_use]
    pub(crate) fn dispatch(&self, call: &MuxCall<'_>) -> String {
        let guarded = panic::catch_unwind(AssertUnwindSafe(|| self.dispatch_inner(call)));
        guarded.unwrap_or_else(|_panic| {
            deny_json(
                "IO_ENFORCEMENT_FAILED",
                "capability enforcement failed",
                ErrorOwner::Operator,
                call.name,
            )
        })
    }

    /// The routing + enforcement body, wrapped by [`Self::dispatch`]'s fail-closed guard.
    fn dispatch_inner(&self, call: &MuxCall<'_>) -> String {
        if let Some(route) = self.routes.get(call.name) {
            if let Trust::ScriptControlled(policy) = &route.trust
                && let Err(message) = policy.enforce(call.payload, call.allow_private)
            {
                return ssrf_block_json(call.name, &message);
            }
            // Precedence: local backend, then the per-request fallback, then the builder fallback.
            let backend = route
                .backend
                .as_ref()
                .or(call.fallback)
                .or(self.fallback.as_ref());
            return call_backend(backend, call);
        }
        // An unregistered name has no wrapper/global; a raw `io.call(name, …)` still reaches the
        // fallback (the broker resolves the logical name, or nothing does).
        let backend = call.fallback.or(self.fallback.as_ref());
        call_backend(backend, call)
    }
}

impl fmt::Debug for CapabilityRegistry {
    #[expect(
        clippy::renamed_function_params,
        reason = "descriptive name over the trait's terse `f`, matching the crate's min-ident lint"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.defs.iter().map(CapabilityDef::name).collect();
        formatter
            .debug_struct("CapabilityRegistry")
            .field("defs", &names)
            .field(
                "fallback",
                &self.fallback.as_ref().map(|_egress| "<fallback>"),
            )
            .finish_non_exhaustive()
    }
}

/// Calls the resolved backend, mapping success/failure to the FFI JSON, or reports
/// `EGRESS_UNAVAILABLE` when no backend serves the name.
fn call_backend(backend: Option<&Arc<dyn Egress>>, call: &MuxCall<'_>) -> String {
    backend.map_or_else(
        || egress_unavailable_json(call.name),
        |egress| match egress.call(call.name, call.action, call.payload) {
            Ok(json) => json,
            Err(err) => err.to_tag_json(),
        },
    )
}

/// Builds a `__runlet` tag for an SSRF-blocked script-controlled call (the script chose the
/// target, so the developer owns it).
fn ssrf_block_json(name: &str, message: &str) -> String {
    errors::dynamic_fault_json(&DynamicFault {
        error: message,
        code: "IO_SSRF_BLOCKED",
        retryable: false,
        owner: ErrorOwner::Developer,
        source: name,
        details: None,
    })
}

/// Builds a `__runlet` tag for a registered name that has no backend and no fallback.
fn egress_unavailable_json(name: &str) -> String {
    errors::dynamic_fault_json(&DynamicFault {
        error: "no egress backend is available for this capability",
        code: "EGRESS_UNAVAILABLE",
        retryable: true,
        owner: ErrorOwner::Operator,
        source: name,
        details: None,
    })
}

/// Builds a `__runlet` deny tag (fail-closed / internal enforcement error).
fn deny_json(code: &str, message: &str, owner: ErrorOwner, name: &str) -> String {
    errors::dynamic_fault_json(&DynamicFault {
        error: message,
        code,
        retryable: false,
        owner,
        source: name,
        details: None,
    })
}
