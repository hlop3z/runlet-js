//! `runlet-core`: a reusable, sandboxed JavaScript logic host powered by `QuickJS`.
//!
//! This crate is the hardened execution core extracted from jsbox: pooled `QuickJS`
//! runtimes, the sync-JS→async-I/O bridge, wall-clock/memory/stack sandboxing, the
//! capability-binding pattern, the opaque-secret model, the `{data,error}` envelope, the
//! error taxonomy, and the 5-tier resilience model. It knows nothing about HTTP or any
//! consumer's data model.
//!
//! Consumers (the `runlet` HTTP front, or a non-HTTP scheduler) drive it through the
//! engine entry points re-exported here. The module surface is currently fully public
//! during the workspace extraction; a curated [`LogicHost`]-style facade narrows it once
//! the callable port lands.

// The six driver-backed capabilities (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) are no longer
// baked into core: they moved to the `runlet-caps` preset crate as composable `CapabilityDef`s
// (JS wrapper + `.d.ts` + trust declaration), injected through the capability registry. Only the
// in-engine, code-carrying capabilities (`http`, SSRF-guarded; `s3`, pure signing) stay here.
pub mod breaker;
pub mod bytecode;
pub mod bytesize;
pub mod capability;
pub mod config;
pub mod decimal;
pub mod egress;
pub mod engine;
pub mod errors;
pub mod host;
#[cfg(feature = "http")]
pub mod http;
pub mod metrics;
pub mod modules;
pub mod partition;
pub mod pool;
pub mod registry;
#[cfg(feature = "s3")]
pub mod s3;
pub mod sandbox;
pub mod types;
// The SSRF classifier (`is_private_ip`/`block_private_ip`) is always compiled: the always-on
// capability mux applies it to every `ScriptControlled` egress capability. Only the
// `reqwest`-based connect-time resolver inside is gated to the in-engine `http`/`s3` clients.
pub mod ssrf;
pub mod sys;

// ── Curated public port ──────────────────────────────────────────────────────
// The blessed entry point; the module surface above stays public during the
// extraction but consumers should prefer these.
pub use crate::capability::{
    CapabilityDef, CapabilityRegistry, RegistryError, SsrfPolicy, Target, TargetExtractor, Trust,
};
pub use crate::config::EngineConfig;
pub use crate::egress::{Egress, EgressError};
pub use crate::engine::{Effect, EngineError, ExecOutcome, Gate, LogEntry, LogLevel, Profile};
pub use crate::host::{
    CapabilitySet, CodeRef, ExecMetrics, HostSettings, Invocation, LogicHost, Outcome,
};
#[cfg(feature = "http")]
pub use crate::http::check_local_egress_url;
pub use crate::pool::PoolStats;
pub use crate::types::{
    BASE_TYPES_DTS, HTTP_TYPES_DTS, S3_TYPES_DTS, def_fragments, generate_types_dts,
};
