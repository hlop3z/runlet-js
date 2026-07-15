//! The box↔broker egress wire contract: the per-capability metric types, the session
//! protocol, and the length-prefixed JSON framing.
//!
//! This lives in `runlet-wire` (not `fabric-backends`) so the sandbox box links it **without** any
//! driver: after the trust flip the box sends only logical resource *names*, then reads back
//! results and metrics, while the broker (which links the drivers) resolves the names to operator
//! configs. One client connection = one box-request session — an `Init` (a **flat list of logical
//! names** + deadline), then one `Call` (name, action, payload) per `io.call(...)`, then a `Drain`
//! for the metrics. See `docs/design/resource-egress.md`.
//!
//! **BREAKING cross-repo wire change (byo-capabilities, D3):** [`WireInit`] carries a flat
//! `resources: Vec<String>` instead of six per-kind `Option<String>` slots — the box is now
//! kind-blind and the broker resolves name → kind → endpoint → creds. This is a deliberate,
//! non-additive break coordinated in lockstep with the reference broker (`fabricd`, sibling repo);
//! an older broker cannot parse the new frame and vice versa.

use std::fmt::{self, Formatter};
use std::io::{Error as IoError, ErrorKind};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::egress::{Egress, EgressError};

/// Hard cap on a single frame's payload (64 MiB) — a malformed/hostile length never allocates
/// without bound.
const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

// -- Per-capability metric types --------------------------------------------
//
// Plain serde data (no driver types), so they live here and the box can deserialize them into the
// response `meta.<cap>_requests` without linking the drivers. The broker's backends construct them.

/// Metric recorded for each `db` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbMetric {
    /// Operation type.
    pub action: String,
    /// Duration in microseconds.
    pub duration_us: u128,
    /// Rows returned (query only).
    pub rows_returned: usize,
    /// Rows affected (execute only).
    pub rows_affected: u64,
    /// Whether the result was truncated.
    pub truncated: bool,
}

/// Metric recorded for each `mongo` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongoMetric {
    /// Operation type.
    pub action: String,
    /// Duration in microseconds.
    pub duration_us: u128,
    /// Documents returned (reads).
    pub docs_returned: usize,
    /// Documents inserted / matched-modified / deleted (writes).
    pub docs_affected: u64,
    /// Whether a read result was truncated at `max_docs`.
    pub truncated: bool,
}

/// Metric recorded for each `mail` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailMetric {
    /// Operation type.
    pub action: String,
    /// Duration in microseconds.
    pub duration_us: u128,
    /// Number of recipients (to + cc + bcc).
    pub recipients: usize,
    /// Serialized message size in bytes.
    pub bytes: usize,
    /// Whether the send was accepted by the server.
    pub accepted: bool,
}

/// Metric recorded for each `redis` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisMetric {
    /// Operation type.
    pub action: String,
    /// Duration in microseconds.
    pub duration_us: u128,
    /// Value size in bytes (get/set; 0 otherwise).
    pub bytes: usize,
    /// Whether a `get` found the key (false otherwise).
    pub hit: bool,
}

/// Metric recorded for each `amq` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmqMetric {
    /// Operation type.
    pub action: String,
    /// Duration in microseconds.
    pub duration_us: u128,
    /// Number of messages in the batch.
    pub messages: usize,
    /// Total payload bytes published.
    pub bytes: usize,
    /// Whether the batch was accepted by the broker.
    pub published: bool,
}

/// Metric recorded for each `auth` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMetric {
    /// Operation type (`user_info` / `introspect`).
    pub action: String,
    /// Issuer host only (no path/query — privacy).
    pub host: String,
    /// IAM HTTP status (0 if the call failed before a response).
    pub status: u16,
    /// Duration in microseconds.
    pub duration_us: u128,
}

/// Generates the `duration_us()` accessor (for the per-capability latency histogram) for each
/// metric type — kept as a method so call sites read the same as before the type move.
macro_rules! duration_accessor {
    ($($ty:ty),+ $(,)?) => {
        $(impl $ty {
            /// Operation duration in microseconds (for the per-capability latency histogram).
            #[must_use]
            pub const fn duration_us(&self) -> u128 {
                self.duration_us
            }
        })+
    };
}
duration_accessor!(
    DbMetric,
    MongoMetric,
    MailMetric,
    RedisMetric,
    AmqMetric,
    AuthMetric
);

/// The driver-backed capability metrics drained from one session.
///
/// Produced daemon-side from each backend's collector and carried back in
/// [`WireResponse::Metrics`]; the box merges these into the response `meta.<cap>_requests`.
/// `http`/`s3` stay in-engine and are not here.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BackendMetrics {
    /// `db` operation metrics.
    pub db: Vec<DbMetric>,
    /// `mongo` operation metrics.
    pub mongo: Vec<MongoMetric>,
    /// `mail` operation metrics.
    pub mail: Vec<MailMetric>,
    /// `redis` operation metrics.
    pub redis: Vec<RedisMetric>,
    /// `amq` operation metrics.
    pub amq: Vec<AmqMetric>,
    /// `auth` operation metrics.
    pub auth: Vec<AuthMetric>,
}

/// An [`Egress`] that also exposes its drained per-capability metrics.
///
/// Lets a consumer treat any egress (the daemon UDS client, or an embedder's in-process adapter)
/// uniformly: pass it as `dyn Egress` to an invocation, then `drain_metrics()` after the run.
pub trait MeteredEgress: Egress {
    /// The per-capability metrics recorded this session.
    fn drain_metrics(&self) -> BackendMetrics;
}

// -- Session protocol -------------------------------------------------------

/// The session-open message: the flat list of logical resource *names* the request may touch,
/// + deadline.
///
/// The box is **kind-blind** and holds no credentials — it forwards only the logical names the
/// request listed in `config.io`; the broker resolves each name against its operator config (name →
/// kind → endpoint → creds). An empty `resources` = no egress requested this session; `timeout_ms`
/// is the per-execution wall-clock budget (the per-op client-side deadline).
///
/// `Debug` is hand-written to **redact** the secret [`token`](Self::token): the derived impl would
/// print it, and a `WireInit` rides inside [`WireRequest`]'s derived `Debug` — so any accidental
/// `?`-logging of a request frame anywhere would leak the credential. The manual impl shows only
/// whether a token is present.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct WireInit {
    /// The flat list of logical resource names this session may address (the request's `config.io`
    /// allowlist). The box forwards names only; the broker resolves each to its kind/endpoint/creds
    /// operator-side. Never per-kind slots (D3).
    #[serde(default)]
    pub resources: Vec<String>,
    /// Per-execution wall-clock budget in milliseconds (the per-op client-side deadline).
    pub timeout_ms: u64,
    /// The request's **trusted** tenant id (the acting-workspace id the edge authorized), forwarded
    /// so the broker can scope name resolution to that tenant's binding set. `None` on the
    /// single-tenant/loopback path (no trusted identity). Sourced only from the trusted-header
    /// extractor — never from anything the executing script can influence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The request's **trusted** acting subject (the acting-user id the edge authorized), forwarded so
    /// a broker — and any backend it resolves to — can attribute who-did-what, the broker-path analogue
    /// of the box-direct `X-Runlet-Actor` header. The bare subject only (never principal kind, roles, or
    /// any other identity field). `None` on the single-tenant/loopback path (no trusted identity).
    /// Sourced only from the trusted-header extractor — never from anything the executing script can
    /// influence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The **verified** principal kind (`user`/`apikey`/`service`) from the signed identity contract,
    /// forwarded alongside `actor` so a broker/backend can branch on what authenticated (e.g. admit a
    /// `service` writer vs a human). `None` unless a verified contract populated it (the plain
    /// trusted-header path and the single-tenant/loopback path carry none). Sourced only from the
    /// trusted-identity extractor — never from anything the executing script can influence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_kind: Option<String>,
    /// The **verified** acted-for subject (`on_behalf_of`) from the signed identity contract — the
    /// creating human for an api-key principal — forwarded alongside `actor` (which stays the bare
    /// subject, i.e. the key id) so a consumer can attribute the action to both the key and the human.
    /// `None` unless a verified contract populated it (absent for a user/service principal and on the
    /// plain/single-tenant path). Sourced only from the trusted-identity extractor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Opaque client-auth credential proving the box may pull credentials — a static shared
    /// secret or a k8s projected `ServiceAccount` token. `None` on the local UDS path (filesystem
    /// permissions gate it); set on the remote QUIC path, where the broker validates it *before*
    /// resolving any name. Carried in the handshake so it costs no extra round-trip. Treated as a
    /// secret: never logged. See `docs/design/network-fabric.md` (QUIC remote transport).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl fmt::Debug for WireInit {
    #[expect(
        clippy::renamed_function_params,
        reason = "`formatter` reads better than the trait's single-char `f` (min_ident_chars)"
    )]
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireInit")
            .field("resources", &self.resources)
            .field("timeout_ms", &self.timeout_ms)
            .field("tenant", &self.tenant)
            // The actor is trusted-edge identity, not a secret (like `tenant`): print it plainly.
            .field("actor", &self.actor)
            // Principal kind + acted-for subject are trusted-edge identity, not secrets: print plainly.
            .field("principal_kind", &self.principal_kind)
            .field("on_behalf_of", &self.on_behalf_of)
            // The token is a secret: print only its presence, never its value.
            .field("token", &self.token.as_ref().map(|_present| "<redacted>"))
            .finish()
    }
}

/// One egress call: the logical resource name, action, and the script's JSON payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireCall {
    /// Logical resource name (`"orders"`, `"cache"`, …) — the broker resolves it to a kind/backend.
    pub name: String,
    /// Action (`"query"`, `"send"`, …).
    pub action: String,
    /// The script's JSON-encoded arguments (untrusted).
    pub payload: String,
}

/// A request frame from the box to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireRequest {
    /// Open the session with the selected resource names + deadline (sent once, first).
    Init(Box<WireInit>),
    /// Perform one egress call.
    Call(WireCall),
    /// Drain the per-capability metrics accumulated this session (sent last).
    Drain,
}

/// A response frame from the daemon to the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireResponse {
    /// Acknowledges a successful [`WireRequest::Init`] (every requested name resolved).
    Ack,
    /// A requested resource name failed to resolve at `Init` — the box maps `code`
    /// (`RESOURCE_NOT_FOUND` / `RESOURCE_KIND_MISMATCH`) to a `400`.
    InitError {
        /// Stable request-category code.
        code: String,
        /// Human-safe detail.
        message: String,
    },
    /// The result of a [`WireRequest::Call`] — the backend JSON, or the classified egress error
    /// (which the engine renders into the `__runlet` tag exactly as for an in-process call).
    Reply(Result<String, EgressError>),
    /// The drained per-capability metrics, answering [`WireRequest::Drain`].
    Metrics(Box<BackendMetrics>),
    /// A protocol-level failure (e.g. a `Call` before `Init`), carrying a human-safe message.
    ProtocolError(String),
}

// -- Framing ----------------------------------------------------------------

/// Writes one length-prefixed JSON frame (`u32` LE length + the JSON bytes) and flushes.
///
/// # Errors
///
/// Returns an I/O error on a serialization failure, an oversized frame, or a write failure.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), IoError>
where
    W: AsyncWrite + Unpin + Send,
    T: Serialize + Sync + ?Sized,
{
    let bytes =
        serde_json::to_vec(value).map_err(|err| IoError::new(ErrorKind::InvalidData, err))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_err| IoError::new(ErrorKind::InvalidData, "frame too large to encode"))?;
    if len > MAX_FRAME_BYTES {
        return Err(IoError::new(
            ErrorKind::InvalidData,
            "frame exceeds size cap",
        ));
    }
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one length-prefixed JSON frame. Returns `Ok(None)` on a clean EOF at a frame boundary
/// (the peer closed the session), `Ok(Some(value))` otherwise.
///
/// # Errors
///
/// Returns an I/O error on a truncated frame, an over-cap length, or a deserialization failure.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, IoError>
where
    R: AsyncRead + Unpin + Send,
    T: DeserializeOwned,
{
    let mut len_buf = [0_u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_n) => {}
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(IoError::new(
            ErrorKind::InvalidData,
            "frame exceeds size cap",
        ));
    }
    let size = usize::try_from(len)
        .map_err(|_err| IoError::new(ErrorKind::InvalidData, "frame length out of range"))?;
    let mut buf = vec![0_u8; size];
    let _ = reader.read_exact(&mut buf).await?;
    let value =
        serde_json::from_slice(&buf).map_err(|err| IoError::new(ErrorKind::InvalidData, err))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    //! Frame round-trip over an in-memory duplex stream, plus the clean-EOF signal.

    use super::{
        BackendMetrics, WireCall, WireInit, WireRequest, WireResponse, read_frame, write_frame,
    };
    use crate::{EgressError, ErrorOwner};

    /// An `Init` frame carries the flat `resources` name list (+ tenant + actor) intact across a
    /// round-trip.
    #[tokio::test]
    async fn init_frame_carries_flat_resources() {
        let (mut sink, mut source) = tokio::io::duplex(1024);
        let sent = WireRequest::Init(Box::new(WireInit {
            resources: vec!["orders".to_owned(), "cache".to_owned()],
            timeout_ms: 5000,
            tenant: Some("acme".to_owned()),
            actor: Some("u_42".to_owned()),
            principal_kind: None,
            on_behalf_of: None,
            token: None,
        }));
        write_frame(&mut sink, &sent)
            .await
            .unwrap_or_else(|_err| unreachable!("write"));
        let got: WireRequest = read_frame(&mut source)
            .await
            .unwrap_or_else(|_err| unreachable!("read"))
            .unwrap_or_else(|| unreachable!("a frame"));
        match got {
            WireRequest::Init(init) => {
                assert_eq!(
                    init.resources,
                    vec!["orders".to_owned(), "cache".to_owned()]
                );
                assert_eq!(init.tenant.as_deref(), Some("acme"));
                assert_eq!(init.actor.as_deref(), Some("u_42"));
            }
            WireRequest::Call(_) | WireRequest::Drain => unreachable!("expected an Init"),
        }
    }

    /// Back-compat: a `WireInit` with no trusted identity serializes with **neither** the `tenant` nor
    /// the `actor` key present (the `skip_serializing_if` guarantee), so the addition of `actor` cannot
    /// change the bytes an unchanged broker sees on the single-tenant/loopback path. A populated `actor`
    /// round-trips through JSON.
    #[test]
    fn actor_is_additive_and_round_trips() {
        let bare = WireInit {
            resources: vec!["orders".to_owned()],
            timeout_ms: 1000,
            tenant: None,
            actor: None,
            principal_kind: None,
            on_behalf_of: None,
            token: None,
        };
        let json = serde_json::to_string(&bare).unwrap_or_else(|_err| unreachable!("serialize"));
        assert!(
            !json.contains("actor"),
            "absent actor is not serialized: {json}"
        );
        assert!(
            !json.contains("tenant"),
            "absent tenant is not serialized: {json}"
        );
        assert!(
            !json.contains("principal_kind") && !json.contains("on_behalf_of"),
            "absent principal_kind/on_behalf_of are not serialized: {json}"
        );

        // A verified api-key contract populates actor (the key id) + principal_kind + on_behalf_of;
        // all three round-trip alongside each other.
        let populated = WireInit {
            actor: Some("key_9".to_owned()),
            principal_kind: Some("apikey".to_owned()),
            on_behalf_of: Some("u_42".to_owned()),
            ..bare
        };
        let json =
            serde_json::to_string(&populated).unwrap_or_else(|_err| unreachable!("serialize"));
        let back: WireInit =
            serde_json::from_str(&json).unwrap_or_else(|_err| unreachable!("deserialize"));
        assert_eq!(back.actor.as_deref(), Some("key_9"));
        assert_eq!(back.principal_kind.as_deref(), Some("apikey"));
        assert_eq!(back.on_behalf_of.as_deref(), Some("u_42"));
    }

    /// A request frame written then read back is structurally equivalent.
    #[tokio::test]
    async fn request_frame_round_trips() {
        let (mut sink, mut source) = tokio::io::duplex(1024);
        let sent = WireRequest::Call(WireCall {
            name: "db".to_owned(),
            action: "query".to_owned(),
            payload: r#"{"sql":"SELECT 1"}"#.to_owned(),
        });
        write_frame(&mut sink, &sent)
            .await
            .unwrap_or_else(|_err| unreachable!("write"));
        let got: WireRequest = read_frame(&mut source)
            .await
            .unwrap_or_else(|_err| unreachable!("read"))
            .unwrap_or_else(|| unreachable!("a frame"));
        match got {
            WireRequest::Call(call) => {
                assert_eq!(call.name, "db");
                assert_eq!(call.action, "query");
            }
            WireRequest::Init(_) | WireRequest::Drain => unreachable!("expected a Call"),
        }
    }

    /// An `EgressError` survives the `WireResponse::Reply` round-trip with its fields intact.
    #[tokio::test]
    async fn error_reply_round_trips() {
        let (mut sink, mut source) = tokio::io::duplex(1024);
        let err = EgressError::new("db", "DB_TIMEOUT", "boom")
            .retryable()
            .owner(ErrorOwner::Operator);
        write_frame(&mut sink, &WireResponse::Reply(Err(err)))
            .await
            .unwrap_or_else(|_err| unreachable!("write"));
        let got: WireResponse = read_frame(&mut source)
            .await
            .unwrap_or_else(|_err| unreachable!("read"))
            .unwrap_or_else(|| unreachable!("a frame"));
        match got {
            WireResponse::Reply(Err(egress)) => {
                assert_eq!(egress.code, "DB_TIMEOUT");
                assert_eq!(egress.source, "db");
                assert!(egress.retryable);
            }
            WireResponse::Ack
            | WireResponse::InitError { .. }
            | WireResponse::Reply(Ok(_))
            | WireResponse::Metrics(_)
            | WireResponse::ProtocolError(_) => unreachable!("expected an error reply"),
        }
    }

    /// Reading at a clean frame boundary after the writer drops yields `Ok(None)`.
    #[tokio::test]
    async fn clean_eof_is_none() {
        let (sink, mut source) = tokio::io::duplex(1024);
        drop(sink);
        let got: Option<WireResponse> = read_frame(&mut source)
            .await
            .unwrap_or_else(|_err| unreachable!("read"));
        assert!(got.is_none(), "clean EOF returns None");
        assert!(BackendMetrics::default().db.is_empty());
    }
}
