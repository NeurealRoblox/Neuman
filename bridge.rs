#![allow(
    missing_docs,
    reason = "the wire contract is documented in /docs/guides/STUDIO_BRIDGE_README.md and schemas/bridge-protocol.schema.json"
)]

//! Authenticated, loopback-only bridge used by the NeuMan Studio plugin.
//!
//! The bridge deliberately exposes a small protocol instead of a generic RPC
//! surface. Pair credentials only authorize this local protocol; they are not
//! cloud credentials. All native payloads are size and hash checked into a
//! quarantined file before the daemon is notified that they are usable.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path as AxumPath, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, broadcast, mpsc, watch},
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use uuid::Uuid;

/// Current major/minor bridge protocol.
pub const PROTOCOL_VERSION: &str = "1.0";
/// Fixed default discovery address mandated by SPEC-08.
pub const DEFAULT_DISCOVERY_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 34_873);
const DISCOVERY_PATH: &str = "/.well-known/neuman-studio-bridge";
const WEBSOCKET_PATH: &str = "/v1/studio";
const DOWNLOAD_PATH: &str = "/v1/studio/download/{ticket}";

/// Hard limits enforced before allocating or accepting payload data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeLimits {
    /// Largest JSON control message.
    pub max_control_message_bytes: usize,
    /// Largest raw transfer chunk.
    pub max_chunk_bytes: usize,
    /// Largest cell transfer.
    pub max_transfer_bytes: u64,
    /// Maximum concurrent commands declared to the client.
    pub max_commands_in_flight: usize,
}

impl Default for BridgeLimits {
    fn default() -> Self {
        Self {
            max_control_message_bytes: 1024 * 1024,
            max_chunk_bytes: 256 * 1024,
            max_transfer_bytes: 96 * 1024 * 1024,
            max_commands_in_flight: 32,
        }
    }
}

/// Runtime configuration. Secure, loopback-only behavior is the default.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Fixed discovery listener. Must be loopback.
    pub discovery_addr: SocketAddr,
    /// Session listener; port zero requests a random OS port. Must be loopback.
    pub session_addr: SocketAddr,
    /// Directory for quarantined and verified transfer files.
    pub transfer_dir: PathBuf,
    /// Human-readable daemon version.
    pub daemon_version: String,
    /// Pairing challenge lifetime.
    pub challenge_ttl: Duration,
    /// Pairing interaction lifetime.
    pub pairing_ttl: Duration,
    /// Idle heartbeat cadence.
    pub heartbeat_interval: Duration,
    /// Protocol resource limits.
    pub limits: BridgeLimits,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            discovery_addr: DEFAULT_DISCOVERY_ADDR,
            session_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            transfer_dir: std::env::temp_dir().join("neuman-studio-bridge"),
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            challenge_ttl: Duration::from_secs(30),
            pairing_ttl: Duration::from_secs(120),
            heartbeat_interval: Duration::from_secs(10),
            limits: BridgeLimits::default(),
        }
    }
}

/// Exact project/place binding required for mutation messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "wire fields are the named SPEC-08 session context contract"
)]
pub struct SessionBinding {
    pub project_id: String,
    pub workspace_id: String,
    pub universe_id: Option<String>,
    pub place_id: Option<String>,
    pub channel: String,
    pub config_hash: String,
    pub ownership_hash: String,
    pub policy_hash: String,
    pub accepted_art_head: Option<String>,
    pub accepted_state_root: Option<String>,
    pub context_version: u64,
}

/// A verified incoming revision announcement sent to one plugin installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "wire fields are the named SPEC-08 incoming revision contract"
)]
pub struct IncomingRevision {
    pub revision_id: String,
    pub state_root: String,
    pub cell_ids: Vec<String>,
    pub author_display: String,
    pub summary: String,
    /// Detached native cells for a bounded apply command. Large-download
    /// transfer scheduling is performed by the daemon before issuing apply.
    #[serde(default)]
    pub cells: Vec<IncomingCell>,
}

/// One daemon-verified native cell carried by a bounded apply command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "wire fields are the named bounded native-cell contract"
)]
pub struct IncomingCell {
    pub cell_id: String,
    pub parent_path: String,
    pub content_hash: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<IncomingDownload>,
}

/// Short-lived authenticated HTTP download for a large accepted native cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingDownload {
    pub url: String,
    pub token: String,
    pub expires_at: String,
}

/// Verified native upload presented to the durable desktop orchestrator.
#[derive(Clone, Debug)]
pub struct VerifiedTransferInput {
    pub session_id: String,
    pub transfer_id: String,
    pub content_hash: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// Authenticated capture intent presented after its native upload is durable.
#[derive(Clone, Debug)]
pub struct CaptureProposalInput {
    pub session_id: String,
    pub plugin_installation_id: String,
    pub cell_id: String,
    pub transfer_id: String,
    pub slot_path: String,
    pub mutation_epoch: u64,
    pub base_revision: Option<String>,
    pub studio_user_id: Option<String>,
    pub studio_version: Option<String>,
}

/// Durable capture result returned before Studio receives its proposal receipt.
#[derive(Clone, Debug)]
pub struct CaptureProposalCommit {
    pub revision_id: String,
    pub status: String,
    pub state_root: String,
    pub fanout: Option<IncomingRevision>,
}

/// Workspace adapter installed by the desktop around the protocol-only bridge.
#[async_trait]
pub trait StudioBridgeOrchestrator: Send + Sync + std::fmt::Debug {
    /// Imports a verified quarantine file into durable local storage.
    async fn transfer_verified(&self, input: VerifiedTransferInput) -> Result<(), String>;

    /// Commits one capture proposal and optionally returns an accepted fan-out.
    async fn capture_proposal(
        &self,
        input: CaptureProposalInput,
    ) -> Result<CaptureProposalCommit, String>;
}

/// A daemon-observable bridge event. Payload bytes and credentials are omitted.
#[derive(Debug, Clone)]
#[allow(
    missing_docs,
    reason = "event fields are documented by their SPEC-08 wire names"
)]
pub enum BridgeEvent {
    PairingChallengeIssued {
        challenge: String,
        pairing_code: String,
        expires_at: OffsetDateTime,
    },
    PairingRequested {
        challenge: String,
        plugin_installation_id: String,
        studio_user_id: Option<String>,
        studio_version: Option<String>,
        studio_platform: Option<String>,
    },
    Paired {
        plugin_installation_id: String,
    },
    SessionConnected {
        session_id: String,
        plugin_installation_id: String,
    },
    SessionDisconnected {
        session_id: String,
    },
    StudioEvent {
        session_id: String,
        kind: String,
        payload: Value,
    },
    CaptureProposal {
        session_id: String,
        cell_id: String,
        transfer_id: String,
        slot_path: String,
        mutation_epoch: u64,
        base_revision: Option<String>,
    },
    ApplyReceipt {
        session_id: String,
        command_id: String,
        revision_id: String,
        status: String,
        verification: String,
    },
    TransferVerified {
        session_id: String,
        transfer_id: String,
        content_hash: String,
        path: PathBuf,
        size_bytes: u64,
    },
    ProtocolViolation {
        session_id: Option<String>,
        code: &'static str,
    },
}

/// Errors returned by the bridge API.
#[derive(Debug, thiserror::Error)]
#[allow(
    missing_docs,
    reason = "variants map one-to-one to the documented SPEC-08 error codes"
)]
pub enum BridgeError {
    #[error("LBP_DISCOVERY_INVALID: {0}")]
    DiscoveryInvalid(String),
    #[error("LBP_PAIRING_REQUIRED")]
    PairingRequired,
    #[error("LBP_PAIRING_DENIED: {0}")]
    PairingDenied(String),
    #[error("LBP_AUTH_INVALID")]
    AuthInvalid,
    #[error("LBP_PROTOCOL_INCOMPATIBLE")]
    ProtocolIncompatible,
    #[error("LBP_CONTEXT_STALE")]
    ContextStale,
    #[error("LBP_SEQUENCE_GAP: expected {expected}, received {received}")]
    SequenceGap { expected: u64, received: u64 },
    #[error("LBP_REPLAY_DETECTED")]
    ReplayDetected,
    #[error("LBP_MESSAGE_TOO_LARGE")]
    MessageTooLarge,
    #[error("LBP_TRANSFER_REJECTED: {0}")]
    TransferRejected(String),
    #[error("LBP_CHUNK_HASH_MISMATCH")]
    ChunkHashMismatch,
    #[error("LBP_CONTENT_HASH_MISMATCH")]
    ContentHashMismatch,
    #[error("LBP_COMMAND_UNSUPPORTED: {0}")]
    CommandUnsupported(String),
    #[error("LBP_ORCHESTRATION_FAILED: {0}")]
    OrchestrationFailed(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl BridgeError {
    fn code(&self) -> &'static str {
        match self {
            Self::DiscoveryInvalid(_) => "LBP_DISCOVERY_INVALID",
            Self::PairingRequired => "LBP_PAIRING_REQUIRED",
            Self::PairingDenied(_) => "LBP_PAIRING_DENIED",
            Self::AuthInvalid => "LBP_AUTH_INVALID",
            Self::ProtocolIncompatible => "LBP_PROTOCOL_INCOMPATIBLE",
            Self::ContextStale => "LBP_CONTEXT_STALE",
            Self::SequenceGap { .. } => "LBP_SEQUENCE_GAP",
            Self::ReplayDetected => "LBP_REPLAY_DETECTED",
            Self::MessageTooLarge => "LBP_MESSAGE_TOO_LARGE",
            Self::TransferRejected(_) => "LBP_TRANSFER_REJECTED",
            Self::ChunkHashMismatch => "LBP_CHUNK_HASH_MISMATCH",
            Self::ContentHashMismatch => "LBP_CONTENT_HASH_MISMATCH",
            Self::CommandUnsupported(_) => "LBP_COMMAND_UNSUPPORTED",
            Self::OrchestrationFailed(_) => "LBP_ORCHESTRATION_FAILED",
            Self::Io(_) | Self::Json(_) => "LBP_INTERNAL",
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, Self::SequenceGap { .. } | Self::Io(_))
    }

    fn response_status(&self) -> StatusCode {
        match self {
            Self::AuthInvalid | Self::PairingRequired => StatusCode::UNAUTHORIZED,
            Self::PairingDenied(_) => StatusCode::FORBIDDEN,
            Self::MessageTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Io(_) | Self::Json(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    fn payload(&self, correlation_id: Option<&str>) -> Value {
        json!({
            "code": self.code(),
            "category": if self.retryable() { "transient" } else { "validation" },
            "message": self.to_string(),
            "retryable": self.retryable(),
            "details": {},
            "correlationId": correlation_id,
        })
    }
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        (self.response_status(), Json(self.payload(None))).into_response()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryDocument {
    schema_version: &'static str,
    installation_id: String,
    daemon_version: String,
    protocol_min: &'static str,
    protocol_max: &'static str,
    web_socket_url: String,
    pairing_required: bool,
    challenge: String,
    expires_at: String,
}

#[derive(Debug, Clone)]
struct ChallengeRecord {
    pairing_code: String,
    expires_at: OffsetDateTime,
    attempts_by_plugin: HashMap<String, u8>,
    approved_plugins: HashSet<String>,
    used: bool,
}

#[derive(Debug, Clone)]
struct CredentialRecord {
    plugin_installation_id: String,
    digest: [u8; 32],
    revoked: bool,
    expires_at: OffsetDateTime,
}

#[derive(Debug, Default)]
struct PairingRegistry {
    challenges: HashMap<String, ChallengeRecord>,
    credentials: HashMap<String, CredentialRecord>,
}

#[derive(Debug, Clone)]
struct SessionSender {
    plugin_installation_id: String,
    hello: Option<HelloContext>,
    bound_context_version: Option<u64>,
    tx: mpsc::Sender<ServerPush>,
}

#[derive(Debug)]
struct BridgeState {
    config: BridgeConfig,
    installation_id: String,
    web_socket_url: RwLock<String>,
    pairing: Mutex<PairingRegistry>,
    binding: RwLock<Option<SessionBinding>>,
    orchestrator: RwLock<Option<Arc<dyn StudioBridgeOrchestrator>>>,
    downloads: Mutex<HashMap<String, DownloadTicket>>,
    events: broadcast::Sender<BridgeEvent>,
    sessions: Mutex<HashMap<String, SessionSender>>,
}

#[derive(Clone, Debug)]
struct DownloadTicket {
    session_id: String,
    token_digest: [u8; 32],
    bytes: Bytes,
    content_hash: String,
    expires_at: OffsetDateTime,
    remaining_uses: u8,
}

/// Cloneable control handle used by the daemon/desktop layer.
#[derive(Clone, Debug)]
pub struct BridgeHandle {
    state: Arc<BridgeState>,
}

impl BridgeHandle {
    /// Subscribe to security-safe bridge lifecycle and Studio events.
    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.state.events.subscribe()
    }

    /// Installs the desktop's durable workspace adapter. Replacing it is safe;
    /// in-flight calls retain the previous `Arc` until completion.
    pub async fn set_orchestrator(&self, orchestrator: Arc<dyn StudioBridgeOrchestrator>) {
        *self.state.orchestrator.write().await = Some(orchestrator);
    }

    /// Refreshes accepted-head evidence without invalidating an otherwise
    /// identical project/place context.
    pub async fn update_accepted_head(&self, revision_id: String, state_root: String) {
        if let Some(binding) = self.state.binding.write().await.as_mut() {
            binding.accepted_art_head = Some(revision_id);
            binding.accepted_state_root = Some(state_root);
        }
    }

    /// Refreshes accepted-head evidence only if no workspace/context switch
    /// occurred while a remote revision was fetched and verified.
    pub async fn update_accepted_head_if_context(
        &self,
        expected_project_id: &str,
        expected_channel: &str,
        expected_context_version: u64,
        revision_id: String,
        state_root: String,
    ) -> Result<(), BridgeError> {
        let mut binding = self.state.binding.write().await;
        let current = binding.as_mut().ok_or(BridgeError::ContextStale)?;
        if current.project_id != expected_project_id
            || current.channel != expected_channel
            || current.context_version != expected_context_version
        {
            return Err(BridgeError::ContextStale);
        }
        current.accepted_art_head = Some(revision_id);
        current.accepted_state_root = Some(state_root);
        Ok(())
    }

    /// Approve the exact plugin installation for an active pairing challenge.
    pub async fn approve_pairing(
        &self,
        challenge: &str,
        plugin_installation_id: &str,
    ) -> Result<(), BridgeError> {
        validate_scoped_id(plugin_installation_id, "plg_")?;
        let mut pairing = self.state.pairing.lock().await;
        let record = pairing
            .challenges
            .get_mut(challenge)
            .ok_or_else(|| BridgeError::PairingDenied("unknown challenge".to_owned()))?;
        if record.used || record.expires_at <= OffsetDateTime::now_utc() {
            return Err(BridgeError::PairingDenied("expired challenge".to_owned()));
        }
        record
            .approved_plugins
            .insert(plugin_installation_id.to_owned());
        Ok(())
    }

    /// Revoke all local bridge access for a plugin installation.
    pub async fn revoke_plugin(&self, plugin_installation_id: &str) {
        let mut pairing = self.state.pairing.lock().await;
        for credential in pairing.credentials.values_mut() {
            if credential.plugin_installation_id == plugin_installation_id {
                credential.revoked = true;
            }
        }
    }

    /// Replace the mutation binding. Sessions receive a versioned context push.
    pub async fn set_binding(&self, binding: Option<SessionBinding>) {
        *self.state.binding.write().await = binding.clone();
        let mut locked = self.state.sessions.lock().await;
        for session in locked.values_mut() {
            session.bound_context_version = None;
        }
        let sessions = locked.clone();
        drop(locked);
        for session in sessions.into_values() {
            let _ = session
                .tx
                .try_send(ServerPush::ContextChanged(binding.clone()));
        }
    }

    /// Notify connected sessions for a plugin installation of accepted art.
    pub async fn notify_incoming_revision(
        &self,
        plugin_installation_id: &str,
        revision: IncomingRevision,
    ) -> usize {
        let sessions = self.state.sessions.lock().await.clone();
        let mut sent = 0;
        for session in sessions.into_values() {
            if session.plugin_installation_id == plugin_installation_id
                && session
                    .tx
                    .try_send(ServerPush::Incoming(revision.clone()))
                    .is_ok()
            {
                sent += 1;
            }
        }
        sent
    }

    /// Request a safe, preflighted apply in one exact Studio session.
    pub async fn request_apply(
        &self,
        session_id: &str,
        command_id: String,
        idempotency_key: String,
        revision: IncomingRevision,
    ) -> Result<(), BridgeError> {
        validate_scoped_id(&command_id, "op_")?;
        if idempotency_key.len() > 256 || idempotency_key.is_empty() {
            return Err(BridgeError::TransferRejected(
                "invalid idempotency key".to_owned(),
            ));
        }
        let binding = self
            .state
            .binding
            .read()
            .await
            .clone()
            .ok_or(BridgeError::ContextStale)?;
        validate_incoming_revision(&self.state, &revision)?;
        let sessions = self.state.sessions.lock().await;
        let sender = sessions.get(session_id).ok_or(BridgeError::ContextStale)?;
        if sender.bound_context_version != Some(binding.context_version)
            || sender
                .hello
                .as_ref()
                .is_none_or(|hello| !hello_matches_binding(hello, &binding))
        {
            return Err(BridgeError::ContextStale);
        }
        sender
            .tx
            .send(ServerPush::Apply {
                command_id,
                idempotency_key,
                context_version: binding.context_version,
                revision,
            })
            .await
            .map_err(|_| BridgeError::ContextStale)
    }

    /// Announces and queues an accepted revision to every other Studio session.
    pub async fn fanout_accepted_revision(
        &self,
        excluding_session_id: &str,
        revision: IncomingRevision,
    ) -> Result<usize, BridgeError> {
        validate_incoming_revision(&self.state, &revision)?;
        fanout_accepted_revision(&self.state, excluding_session_id, revision).await
    }

    /// Fans a remote accepted revision only while the bridge still has the
    /// exact local project/channel generation for which it was fetched.
    pub async fn fanout_remote_accepted_revision(
        &self,
        expected_project_id: &str,
        expected_channel: &str,
        expected_context_version: u64,
        excluding_session_id: Option<&str>,
        revision: IncomingRevision,
    ) -> Result<usize, BridgeError> {
        let binding = self
            .state
            .binding
            .read()
            .await
            .clone()
            .ok_or(BridgeError::ContextStale)?;
        if binding.project_id != expected_project_id
            || binding.channel != expected_channel
            || binding.context_version != expected_context_version
        {
            return Err(BridgeError::ContextStale);
        }
        validate_incoming_revision(&self.state, &revision)?;
        fanout_accepted_revision(&self.state, excluding_session_id.unwrap_or(""), revision).await
    }
}

fn validate_incoming_revision(
    state: &Arc<BridgeState>,
    revision: &IncomingRevision,
) -> Result<(), BridgeError> {
    if revision.cells.is_empty() || revision.cells.len() > 128 {
        return Err(BridgeError::TransferRejected(
            "accepted revision has no bounded cell payload".to_owned(),
        ));
    }
    let declared_cells: HashSet<&str> = revision.cell_ids.iter().map(String::as_str).collect();
    if declared_cells.len() != revision.cells.len()
        || revision
            .cells
            .iter()
            .any(|cell| !declared_cells.contains(cell.cell_id.as_str()))
    {
        return Err(BridgeError::TransferRejected(
            "incoming summary cell IDs do not match payloads".to_owned(),
        ));
    }
    for cell in &revision.cells {
        validate_scoped_id(&cell.cell_id, "cell_")?;
        validate_blake3_hash(&cell.content_hash)?;
        let data = cell.data.as_ref().ok_or_else(|| {
            BridgeError::TransferRejected("native apply source bytes are missing".to_owned())
        })?;
        if cell.download.is_some() {
            return Err(BridgeError::TransferRejected(
                "caller may not supply a download ticket".to_owned(),
            ));
        }
        if cell.size_bytes > state.config.limits.max_transfer_bytes
            || data.len()
                > state
                    .config
                    .limits
                    .max_transfer_bytes
                    .saturating_mul(4)
                    .div_ceil(3) as usize
                    + 8
        {
            return Err(BridgeError::MessageTooLarge);
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(data.as_bytes())
            .map_err(|_| BridgeError::TransferRejected("invalid apply base64url".to_owned()))?;
        if decoded.len() as u64 != cell.size_bytes || blake3_label(&decoded) != cell.content_hash {
            return Err(BridgeError::ContentHashMismatch);
        }
    }
    Ok(())
}

async fn prepare_revision_for_session(
    state: &Arc<BridgeState>,
    runtime: &SessionRuntime,
    mut revision: IncomingRevision,
) -> Result<IncomingRevision, BridgeError> {
    let inline_limit = state
        .config
        .limits
        .max_control_message_bytes
        .saturating_sub(4096);
    if serde_json::to_vec(&revision)?.len() <= inline_limit {
        return Ok(revision);
    }
    if !runtime
        .hello
        .as_ref()
        .is_some_and(|hello| hello.capabilities.contains("http-download-v1"))
    {
        return Err(BridgeError::CommandUnsupported(
            "http-download-v1 capability is required for this accepted revision".to_owned(),
        ));
    }
    for cell in &mut revision.cells {
        let encoded = cell.data.take().ok_or_else(|| {
            BridgeError::TransferRejected("native apply source bytes are missing".to_owned())
        })?;
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded.as_bytes())
            .map_err(|_| BridgeError::TransferRejected("invalid apply base64url".to_owned()))?;
        if decoded.len() as u64 != cell.size_bytes || blake3_label(&decoded) != cell.content_hash {
            return Err(BridgeError::ContentHashMismatch);
        }
        let ticket_id = format!("dwn_{}", Uuid::new_v4());
        let token = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
        let expires_at = OffsetDateTime::now_utc() + Duration::from_secs(120);
        let mut url = url::Url::parse(&state.web_socket_url.read().await)
            .map_err(|_| BridgeError::DiscoveryInvalid("invalid session URL".to_owned()))?;
        url.set_scheme("http")
            .map_err(|_| BridgeError::DiscoveryInvalid("invalid download scheme".to_owned()))?;
        url.set_path(&format!("/v1/studio/download/{ticket_id}"));
        url.set_query(None);
        url.set_fragment(None);
        state.downloads.lock().await.insert(
            ticket_id,
            DownloadTicket {
                session_id: runtime.session_id.clone(),
                token_digest: digest_secret(&token),
                bytes: Bytes::from(decoded),
                content_hash: cell.content_hash.clone(),
                expires_at,
                remaining_uses: 3,
            },
        );
        cell.download = Some(IncomingDownload {
            url: url.to_string(),
            token,
            expires_at: expires_at.format(&Rfc3339).unwrap_or_default(),
        });
    }
    if serde_json::to_vec(&revision)?.len() > inline_limit {
        return Err(BridgeError::MessageTooLarge);
    }
    Ok(revision)
}

async fn fanout_accepted_revision(
    state: &Arc<BridgeState>,
    excluding_session_id: &str,
    revision: IncomingRevision,
) -> Result<usize, BridgeError> {
    let binding = state
        .binding
        .read()
        .await
        .clone()
        .ok_or(BridgeError::ContextStale)?;
    let context_version = binding.context_version;
    let sessions = state.sessions.lock().await.clone();
    let mut sent = 0;
    for (session_id, session) in sessions {
        if session_id == excluding_session_id
            || session.bound_context_version != Some(context_version)
            || session
                .hello
                .as_ref()
                .is_none_or(|hello| !hello_matches_binding(hello, &binding))
        {
            continue;
        }
        let idempotency_key = format!("apply:{}:{session_id}", revision.revision_id);
        let command_id = format!("op_{}", Uuid::now_v7());
        if session
            .tx
            .send(ServerPush::IncomingAndApply {
                command_id,
                idempotency_key,
                context_version,
                revision: revision.clone(),
            })
            .await
            .is_ok()
        {
            sent += 1;
        }
    }
    Ok(sent)
}

/// Running listeners and their daemon control handle.
#[derive(Debug)]
pub struct RunningBridge {
    /// Daemon-facing control and event handle.
    pub handle: BridgeHandle,
    /// Actual bound discovery endpoint.
    pub discovery_addr: SocketAddr,
    /// Actual bound WebSocket endpoint.
    pub session_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningBridge {
    /// Stop both listeners and wait for their serve loops.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// Loopback bridge service factory.
pub struct BridgeService;

impl BridgeService {
    /// Bind discovery and WebSocket listeners. Non-loopback addresses fail closed.
    pub async fn start(mut config: BridgeConfig) -> Result<RunningBridge, BridgeError> {
        if !config.discovery_addr.ip().is_loopback() || !config.session_addr.ip().is_loopback() {
            return Err(BridgeError::DiscoveryInvalid(
                "listeners must use numeric loopback addresses".to_owned(),
            ));
        }
        if config.limits.max_chunk_bytes == 0
            || config.limits.max_control_message_bytes < 4096
            || config.limits.max_transfer_bytes < config.limits.max_chunk_bytes as u64
        {
            return Err(BridgeError::DiscoveryInvalid(
                "invalid resource limits".to_owned(),
            ));
        }
        fs::create_dir_all(&config.transfer_dir)?;

        let discovery_listener = TcpListener::bind(config.discovery_addr).await?;
        let session_listener = TcpListener::bind(config.session_addr).await?;
        let discovery_addr = discovery_listener.local_addr()?;
        let session_addr = session_listener.local_addr()?;
        config.discovery_addr = discovery_addr;
        config.session_addr = session_addr;
        let ws_host = if session_addr.ip().is_ipv6() {
            format!("[{}]", session_addr.ip())
        } else {
            session_addr.ip().to_string()
        };
        let ws_url = format!("ws://{ws_host}:{}{WEBSOCKET_PATH}", session_addr.port());
        let (events, _) = broadcast::channel(256);
        let state = Arc::new(BridgeState {
            config,
            installation_id: format!("ins_{}", Uuid::new_v4()),
            web_socket_url: RwLock::new(ws_url),
            pairing: Mutex::new(PairingRegistry::default()),
            binding: RwLock::new(None),
            orchestrator: RwLock::new(None),
            downloads: Mutex::new(HashMap::new()),
            events,
            sessions: Mutex::new(HashMap::new()),
        });

        let discovery_router = Router::new()
            .route(DISCOVERY_PATH, get(discovery_handler))
            .fallback(|| async { StatusCode::NOT_FOUND })
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(state.clone());
        let session_router = Router::new()
            .route(WEBSOCKET_PATH, get(websocket_handler))
            .route(DOWNLOAD_PATH, get(download_handler))
            .fallback(|| async { StatusCode::NOT_FOUND })
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(state.clone());

        let (shutdown, mut discovery_shutdown) = watch::channel(false);
        let mut session_shutdown = shutdown.subscribe();
        let discovery_task = tokio::spawn(async move {
            let result = axum::serve(discovery_listener, discovery_router)
                .with_graceful_shutdown(async move {
                    while discovery_shutdown.changed().await.is_ok() {
                        if *discovery_shutdown.borrow() {
                            break;
                        }
                    }
                })
                .await;
            if let Err(error) = result {
                tracing::error!(%error, "discovery listener failed");
            }
        });
        let session_task = tokio::spawn(async move {
            let result = axum::serve(session_listener, session_router)
                .with_graceful_shutdown(async move {
                    while session_shutdown.changed().await.is_ok() {
                        if *session_shutdown.borrow() {
                            break;
                        }
                    }
                })
                .await;
            if let Err(error) = result {
                tracing::error!(%error, "session listener failed");
            }
        });

        Ok(RunningBridge {
            handle: BridgeHandle { state },
            discovery_addr,
            session_addr,
            shutdown,
            tasks: vec![discovery_task, session_task],
        })
    }
}

async fn discovery_handler(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
) -> Result<Response, BridgeError> {
    validate_host_header(&headers, state.config.discovery_addr.port())?;
    let (challenge, expires_at) = issue_challenge(&state).await;
    let response = DiscoveryDocument {
        schema_version: PROTOCOL_VERSION,
        installation_id: state.installation_id.clone(),
        daemon_version: state.config.daemon_version.clone(),
        protocol_min: PROTOCOL_VERSION,
        protocol_max: PROTOCOL_VERSION,
        web_socket_url: state.web_socket_url.read().await.clone(),
        pairing_required: true,
        challenge,
        expires_at: expires_at.format(&Rfc3339).unwrap_or_default(),
    };
    let mut response = Json(response).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/json".parse().expect("static header"),
    );
    Ok(response)
}

async fn issue_challenge(state: &Arc<BridgeState>) -> (String, OffsetDateTime) {
    let mut random = [0_u8; 24];
    rand::rng().fill_bytes(&mut random);
    let challenge = URL_SAFE_NO_PAD.encode(random);
    let mut code_bytes = [0_u8; 4];
    rand::rng().fill_bytes(&mut code_bytes);
    let code_number = u32::from_le_bytes(code_bytes) % 1_000_000;
    let pairing_code = format!("{code_number:06}");
    let expires_at = OffsetDateTime::now_utc() + state.config.challenge_ttl;
    let record = ChallengeRecord {
        pairing_code: pairing_code.clone(),
        expires_at,
        attempts_by_plugin: HashMap::new(),
        approved_plugins: HashSet::new(),
        used: false,
    };
    let mut pairing = state.pairing.lock().await;
    let now = OffsetDateTime::now_utc();
    pairing
        .challenges
        .retain(|_, record| !record.used && record.expires_at > now);
    pairing.challenges.insert(challenge.clone(), record);
    drop(pairing);
    let _ = state.events.send(BridgeEvent::PairingChallengeIssued {
        challenge: challenge.clone(),
        pairing_code,
        expires_at,
    });
    (challenge, expires_at)
}

async fn websocket_handler(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, BridgeError> {
    validate_host_header_from_url(&headers, &state.web_socket_url.read().await)?;
    let header_auth = header_authentication(&state, &headers).await;
    let max = state.config.limits.max_control_message_bytes;
    Ok(ws
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| session_loop(state, socket, header_auth)))
}

async fn download_handler(
    State(state): State<Arc<BridgeState>>,
    AxumPath(ticket_id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, BridgeError> {
    validate_host_header_from_url(&headers, &state.web_socket_url.read().await)?;
    validate_scoped_id(&ticket_id, "dwn_")?;
    let session_id = headers
        .get("x-neuman-session")
        .and_then(|value| value.to_str().ok())
        .ok_or(BridgeError::AuthInvalid)?;
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("NeuManTransfer "))
        .ok_or(BridgeError::AuthInvalid)?;
    let token_digest = digest_secret(authorization);
    let now = OffsetDateTime::now_utc();
    let mut downloads = state.downloads.lock().await;
    downloads.retain(|_, ticket| ticket.expires_at > now && ticket.remaining_uses > 0);
    let (bytes, content_hash, expires_at, remove) = {
        let ticket = downloads
            .get_mut(&ticket_id)
            .ok_or(BridgeError::AuthInvalid)?;
        if ticket.session_id != session_id
            || !bool::from(ticket.token_digest.ct_eq(&token_digest))
            || ticket.expires_at <= now
        {
            return Err(BridgeError::AuthInvalid);
        }
        ticket.remaining_uses = ticket.remaining_uses.saturating_sub(1);
        (
            ticket.bytes.clone(),
            ticket.content_hash.clone(),
            ticket.expires_at,
            ticket.remaining_uses == 0,
        )
    };
    if remove {
        downloads.remove(&ticket_id);
    }
    drop(downloads);
    let mut response = Response::new(Body::from(bytes.clone()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-roblox-rbxm".parse().expect("static header"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response.headers_mut().insert(
        "x-neuman-content-hash",
        content_hash
            .parse()
            .map_err(|_| BridgeError::ContentHashMismatch)?,
    );
    response.headers_mut().insert(
        "x-neuman-size",
        bytes
            .len()
            .to_string()
            .parse()
            .map_err(|_| BridgeError::MessageTooLarge)?,
    );
    response.headers_mut().insert(
        "x-neuman-ticket-expires",
        expires_at
            .format(&Rfc3339)
            .unwrap_or_default()
            .parse()
            .map_err(|_| BridgeError::AuthInvalid)?,
    );
    Ok(response)
}

fn validate_host_header(headers: &HeaderMap, expected_port: u16) -> Result<(), BridgeError> {
    let raw = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| BridgeError::DiscoveryInvalid("missing Host header".to_owned()))?;
    let authority: http::uri::Authority = raw
        .parse()
        .map_err(|_| BridgeError::DiscoveryInvalid("invalid Host header".to_owned()))?;
    let host = authority.host();
    let ip: IpAddr = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| BridgeError::DiscoveryInvalid("Host must be numeric loopback".to_owned()))?;
    if !ip.is_loopback() || authority.port_u16().unwrap_or(expected_port) != expected_port {
        return Err(BridgeError::DiscoveryInvalid(
            "Host is not the bound loopback endpoint".to_owned(),
        ));
    }
    Ok(())
}

fn validate_host_header_from_url(headers: &HeaderMap, url: &str) -> Result<(), BridgeError> {
    let parsed = url::Url::parse(url)
        .map_err(|_| BridgeError::DiscoveryInvalid("invalid session URL".to_owned()))?;
    validate_host_header(headers, parsed.port().unwrap_or(80))
}

async fn header_authentication(state: &Arc<BridgeState>, headers: &HeaderMap) -> Option<String> {
    let plugin_id = headers
        .get("x-neuman-plugin-installation")
        .and_then(|value| value.to_str().ok())?;
    let protocol = headers
        .get("x-neuman-protocol")
        .and_then(|value| value.to_str().ok())?;
    if negotiate_protocol(protocol, protocol).is_err() {
        return None;
    }
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    let credential = authorization.strip_prefix("NeuManPair ")?;
    authenticate_credential(state, plugin_id, credential)
        .await
        .ok()
        .map(|()| plugin_id.to_owned())
}

fn negotiate_protocol(minimum: &str, maximum: &str) -> Result<&'static str, BridgeError> {
    let parse = |version: &str| -> Option<(u64, u64)> {
        let (major, minor) = version.split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    };
    let min = parse(minimum).ok_or(BridgeError::ProtocolIncompatible)?;
    let max = parse(maximum).ok_or(BridgeError::ProtocolIncompatible)?;
    if min.0 != 1 || max.0 != 1 || min > (1, 0) || max < (1, 0) || min > max {
        return Err(BridgeError::ProtocolIncompatible);
    }
    Ok(PROTOCOL_VERSION)
}

fn validate_scoped_id(value: &str, prefix: &str) -> Result<(), BridgeError> {
    if value.len() < prefix.len() + 4
        || value.len() > 128
        || !value.starts_with(prefix)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(BridgeError::TransferRejected(format!(
            "invalid identifier for {prefix}"
        )));
    }
    Ok(())
}

fn digest_secret(secret: &str) -> [u8; 32] {
    *blake3::hash(secret.as_bytes()).as_bytes()
}

async fn authenticate_credential(
    state: &Arc<BridgeState>,
    plugin_id: &str,
    credential: &str,
) -> Result<(), BridgeError> {
    validate_scoped_id(plugin_id, "plg_").map_err(|_| BridgeError::AuthInvalid)?;
    if credential.len() < 32 || credential.len() > 256 {
        return Err(BridgeError::AuthInvalid);
    }
    let digest = digest_secret(credential);
    let pairing = state.pairing.lock().await;
    let valid = pairing.credentials.values().any(|record| {
        record.plugin_installation_id == plugin_id
            && !record.revoked
            && record.expires_at > OffsetDateTime::now_utc()
            && bool::from(record.digest.ct_eq(&digest))
    });
    if valid {
        Ok(())
    } else {
        Err(BridgeError::AuthInvalid)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    challenge: String,
    pairing_code: String,
    plugin_installation_id: String,
    plugin_version: String,
    protocol_version: String,
    #[serde(default)]
    studio: StudioIdentity,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StudioIdentity {
    user_id: Option<String>,
    version: Option<String>,
    platform: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticateRequest {
    credential: String,
    plugin_installation_id: String,
    protocol_min: String,
    protocol_max: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    protocol_version: String,
    #[serde(rename = "type")]
    kind: String,
    message_id: String,
    #[serde(default)]
    correlation_id: Option<String>,
    sequence: u64,
    sent_at: String,
    session_id: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboundEnvelope<'a> {
    protocol_version: &'static str,
    #[serde(rename = "type")]
    kind: &'a str,
    message_id: String,
    correlation_id: Option<String>,
    sequence: u64,
    sent_at: String,
    session_id: &'a str,
    payload: Value,
}

#[derive(Debug, Clone)]
enum ServerPush {
    ContextChanged(Option<SessionBinding>),
    Incoming(IncomingRevision),
    Apply {
        command_id: String,
        idempotency_key: String,
        context_version: u64,
        revision: IncomingRevision,
    },
    IncomingAndApply {
        command_id: String,
        idempotency_key: String,
        context_version: u64,
        revision: IncomingRevision,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum SequenceDecision {
    New,
    Duplicate,
}

#[derive(Debug)]
struct SequenceGuard {
    expected: u64,
    seen: HashMap<String, [u8; 32]>,
    order: Vec<String>,
}

impl SequenceGuard {
    fn new() -> Self {
        Self {
            expected: 1,
            seen: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn check(&mut self, envelope: &Envelope, raw: &[u8]) -> Result<SequenceDecision, BridgeError> {
        if envelope.message_id.len() > 128 || envelope.message_id.is_empty() {
            return Err(BridgeError::ReplayDetected);
        }
        let digest = *blake3::hash(raw).as_bytes();
        if let Some(previous) = self.seen.get(&envelope.message_id) {
            if bool::from(previous.ct_eq(&digest)) && envelope.sequence < self.expected {
                return Ok(SequenceDecision::Duplicate);
            }
            return Err(BridgeError::ReplayDetected);
        }
        if envelope.sequence != self.expected {
            return Err(BridgeError::SequenceGap {
                expected: self.expected,
                received: envelope.sequence,
            });
        }
        self.expected += 1;
        self.seen.insert(envelope.message_id.clone(), digest);
        self.order.push(envelope.message_id.clone());
        if self.order.len() > 1024
            && let Some(expired) = self.order.first().cloned()
        {
            self.order.remove(0);
            self.seen.remove(&expired);
        }
        Ok(SequenceDecision::New)
    }
}

#[derive(Debug)]
struct SessionRuntime {
    session_id: String,
    incoming_sequence: SequenceGuard,
    outgoing_sequence: u64,
    transfer: TransferManager,
    hello: Option<HelloContext>,
    command_receipts: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
struct HelloContext {
    universe_id: Option<String>,
    place_id: Option<String>,
    project_id: Option<String>,
    channel: Option<String>,
    studio_user_id: Option<String>,
    studio_version: Option<String>,
    capabilities: HashSet<String>,
}

impl SessionRuntime {
    fn next_envelope(&mut self, kind: &str, payload: Value, correlation: Option<String>) -> String {
        let envelope = OutboundEnvelope {
            protocol_version: PROTOCOL_VERSION,
            kind,
            message_id: format!("msg_{}", Uuid::now_v7()),
            correlation_id: correlation,
            sequence: self.outgoing_sequence,
            sent_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            session_id: &self.session_id,
            payload,
        };
        self.outgoing_sequence += 1;
        serde_json::to_string(&envelope).expect("outbound envelope is serializable")
    }
}

async fn session_loop(state: Arc<BridgeState>, mut socket: WebSocket, header_auth: Option<String>) {
    let authenticated = if let Some(plugin_id) = header_auth {
        Some(plugin_id)
    } else {
        authenticate_first_frame(&state, &mut socket).await
    };
    let Some(plugin_id) = authenticated else {
        let _ = socket.close().await;
        return;
    };

    let session_id = format!("ses_{}", Uuid::new_v4());
    let auth_ok = json!({
        "type": "session.authenticated",
        "protocolVersion": PROTOCOL_VERSION,
        "sessionId": session_id,
        "sessionToken": URL_SAFE_NO_PAD.encode(random_bytes::<32>()),
    });
    if socket
        .send(Message::Text(auth_ok.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let (push_tx, mut push_rx) = mpsc::channel(32);
    state.sessions.lock().await.insert(
        session_id.clone(),
        SessionSender {
            plugin_installation_id: plugin_id.clone(),
            hello: None,
            bound_context_version: None,
            tx: push_tx,
        },
    );
    let _ = state.events.send(BridgeEvent::SessionConnected {
        session_id: session_id.clone(),
        plugin_installation_id: plugin_id.clone(),
    });

    let mut runtime = SessionRuntime {
        session_id: session_id.clone(),
        incoming_sequence: SequenceGuard::new(),
        outgoing_sequence: 1,
        transfer: TransferManager::new(
            state.config.transfer_dir.clone(),
            state.config.limits.clone(),
            session_id.clone(),
        ),
        hello: None,
        command_receipts: HashMap::new(),
    };
    let mut heartbeat = interval(state.config.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut missed_pongs = 0_u8;

    loop {
        tokio::select! {
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > state.config.limits.max_control_message_bytes {
                            let _ = send_error(&mut socket, &mut runtime, BridgeError::MessageTooLarge, None).await;
                            break;
                        }
                        if serde_json::from_str::<Value>(&text)
                            .ok()
                            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
                            .as_deref()
                            == Some("session.pong")
                        {
                            missed_pongs = 0;
                        }
                        match process_envelope(&state, &mut runtime, text.as_bytes()).await {
                            Ok(Some((kind, payload, correlation))) => {
                                let outgoing = runtime.next_envelope(&kind, payload, correlation);
                                if socket.send(Message::Text(outgoing.into())).await.is_err() { break; }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                let security = matches!(
                                    error,
                                    BridgeError::ReplayDetected | BridgeError::SequenceGap { .. }
                                );
                                let _ = state.events.send(BridgeEvent::ProtocolViolation { session_id: Some(session_id.clone()), code: error.code() });
                                let _ = send_error(&mut socket, &mut runtime, error, None).await;
                                if security { break; }
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => missed_pongs = 0,
                    Some(Ok(Message::Ping(bytes))) => {
                        if socket.send(Message::Pong(bytes)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Binary(_))) => {
                        let _ = send_error(&mut socket, &mut runtime, BridgeError::CommandUnsupported("binary frames not negotiated".to_owned()), None).await;
                        break;
                    }
                }
            }
            push = push_rx.recv() => {
                let Some(push) = push else { break; };
                match push {
                    ServerPush::IncomingAndApply { command_id, idempotency_key, context_version, revision } => {
                        let revision = match prepare_revision_for_session(&state, &runtime, revision).await {
                            Ok(revision) => revision,
                            Err(error) => {
                                let _ = send_error(&mut socket, &mut runtime, error, None).await;
                                continue;
                            }
                        };
                        let incoming = runtime.next_envelope("art.incoming-summary", incoming_summary_payload(&revision), None);
                        if socket.send(Message::Text(incoming.into())).await.is_err() { break; }
                        let apply = runtime.next_envelope(
                            "command.request",
                            apply_command_payload(command_id, idempotency_key, context_version, revision),
                            None,
                        );
                        if socket.send(Message::Text(apply.into())).await.is_err() { break; }
                    }
                    ServerPush::Apply { command_id, idempotency_key, context_version, revision } => {
                        let revision = match prepare_revision_for_session(&state, &runtime, revision).await {
                            Ok(revision) => revision,
                            Err(error) => {
                                let _ = send_error(&mut socket, &mut runtime, error, None).await;
                                continue;
                            }
                        };
                        let apply = runtime.next_envelope(
                            "command.request",
                            apply_command_payload(command_id, idempotency_key, context_version, revision),
                            None,
                        );
                        if socket.send(Message::Text(apply.into())).await.is_err() { break; }
                    }
                    other => {
                        let (kind, payload) = server_push_payload(other);
                        let outgoing = runtime.next_envelope(kind, payload, None);
                        if socket.send(Message::Text(outgoing.into())).await.is_err() { break; }
                    }
                }
            }
            _ = heartbeat.tick() => {
                missed_pongs = missed_pongs.saturating_add(1);
                if missed_pongs > 3 { break; }
                let outgoing = runtime.next_envelope("session.ping", json!({"missed": missed_pongs - 1}), None);
                if socket.send(Message::Text(outgoing.into())).await.is_err() { break; }
            }
        }
    }

    runtime.transfer.cancel_all();
    state.sessions.lock().await.remove(&session_id);
    state
        .downloads
        .lock()
        .await
        .retain(|_, ticket| ticket.session_id != session_id);
    let _ = state
        .events
        .send(BridgeEvent::SessionDisconnected { session_id });
    let _ = socket.close().await;
}

fn server_push_payload(push: ServerPush) -> (&'static str, Value) {
    match push {
        ServerPush::ContextChanged(binding) => (
            "session.context-changed",
            json!({
                "bound": false,
                "binding": binding,
                "requiresRefresh": true,
                "unboundReason": "context-changed-revalidation-required",
            }),
        ),
        ServerPush::Incoming(revision) => ("art.incoming-summary", json!(revision)),
        ServerPush::Apply { .. } | ServerPush::IncomingAndApply { .. } => {
            unreachable!("handled by session loop")
        }
    }
}

fn incoming_summary_payload(revision: &IncomingRevision) -> Value {
    json!({
        "revisionId": revision.revision_id,
        "stateRoot": revision.state_root,
        "cellIds": revision.cell_ids,
        "authorDisplay": revision.author_display,
        "summary": revision.summary,
    })
}

fn apply_command_payload(
    command_id: String,
    idempotency_key: String,
    context_version: u64,
    revision: IncomingRevision,
) -> Value {
    json!({
        "commandId": command_id,
        "command": "art.apply",
        "contextVersion": context_version,
        "idempotencyKey": idempotency_key,
        "deadlineAt": (OffsetDateTime::now_utc() + Duration::from_secs(300)).format(&Rfc3339).unwrap_or_default(),
        "arguments": revision,
    })
}

async fn send_error(
    socket: &mut WebSocket,
    runtime: &mut SessionRuntime,
    error: BridgeError,
    correlation: Option<String>,
) -> Result<(), axum::Error> {
    let payload = error.payload(correlation.as_deref());
    let message = runtime.next_envelope("protocol.error", payload, correlation);
    socket.send(Message::Text(message.into())).await
}

async fn authenticate_first_frame(
    state: &Arc<BridgeState>,
    socket: &mut WebSocket,
) -> Option<String> {
    let next = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .ok()??
        .ok()?;
    let Message::Text(text) = next else {
        return None;
    };
    if text.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_str(&text).ok()?;
    match value.get("type").and_then(Value::as_str) {
        Some("session.authenticate") => {
            let request: AuthenticateRequest = serde_json::from_value(value).ok()?;
            negotiate_protocol(&request.protocol_min, &request.protocol_max).ok()?;
            authenticate_credential(state, &request.plugin_installation_id, &request.credential)
                .await
                .ok()?;
            Some(request.plugin_installation_id)
        }
        Some("pair.request") => {
            let request: PairRequest = serde_json::from_value(value).ok()?;
            let result = complete_pairing(state, request).await;
            let payload = match result {
                Ok(success) => json!({
                    "type": "pair.succeeded",
                    "installationId": state.installation_id,
                    "protocolVersion": PROTOCOL_VERSION,
                    "credential": success.credential,
                    "expiresAt": success.expires_at.format(&Rfc3339).unwrap_or_default(),
                    "renewable": true,
                }),
                Err(error) => json!({
                    "type": if matches!(error, BridgeError::PairingRequired) { "pair.pending" } else { "pair.failed" },
                    "error": error.payload(None),
                }),
            };
            let _ = socket.send(Message::Text(payload.to_string().into())).await;
            None
        }
        _ => None,
    }
}

struct PairSuccess {
    credential: String,
    expires_at: OffsetDateTime,
}

async fn complete_pairing(
    state: &Arc<BridgeState>,
    request: PairRequest,
) -> Result<PairSuccess, BridgeError> {
    validate_scoped_id(&request.plugin_installation_id, "plg_")?;
    if request.plugin_version.len() > 64 || request.pairing_code.len() != 6 {
        return Err(BridgeError::PairingDenied(
            "invalid pairing request".to_owned(),
        ));
    }
    negotiate_protocol(&request.protocol_version, &request.protocol_version)?;
    let mut pairing = state.pairing.lock().await;
    let record = pairing
        .challenges
        .get_mut(&request.challenge)
        .ok_or_else(|| BridgeError::PairingDenied("unknown challenge".to_owned()))?;
    if record.used || record.expires_at <= OffsetDateTime::now_utc() {
        return Err(BridgeError::PairingDenied("expired challenge".to_owned()));
    }
    let attempts = record
        .attempts_by_plugin
        .entry(request.plugin_installation_id.clone())
        .or_default();
    if *attempts >= 5 {
        return Err(BridgeError::PairingDenied(
            "attempt limit exceeded".to_owned(),
        ));
    }
    *attempts += 1;
    if !bool::from(
        record
            .pairing_code
            .as_bytes()
            .ct_eq(request.pairing_code.as_bytes()),
    ) {
        return Err(BridgeError::PairingDenied("code mismatch".to_owned()));
    }
    if !record
        .approved_plugins
        .contains(&request.plugin_installation_id)
    {
        drop(pairing);
        let _ = state.events.send(BridgeEvent::PairingRequested {
            challenge: request.challenge,
            plugin_installation_id: request.plugin_installation_id,
            studio_user_id: request.studio.user_id,
            studio_version: request.studio.version,
            studio_platform: request.studio.platform,
        });
        return Err(BridgeError::PairingRequired);
    }
    record.used = true;
    let credential = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
    let expires_at = OffsetDateTime::now_utc() + Duration::from_secs(30 * 24 * 60 * 60);
    pairing.credentials.insert(
        format!("cred_{}", Uuid::new_v4()),
        CredentialRecord {
            plugin_installation_id: request.plugin_installation_id.clone(),
            digest: digest_secret(&credential),
            revoked: false,
            expires_at,
        },
    );
    drop(pairing);
    let _ = state.events.send(BridgeEvent::Paired {
        plugin_installation_id: request.plugin_installation_id,
    });
    Ok(PairSuccess {
        credential,
        expires_at,
    })
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

async fn process_envelope(
    state: &Arc<BridgeState>,
    runtime: &mut SessionRuntime,
    raw: &[u8],
) -> Result<Option<(String, Value, Option<String>)>, BridgeError> {
    let envelope: Envelope = serde_json::from_slice(raw)?;
    if envelope.protocol_version != PROTOCOL_VERSION || envelope.session_id != runtime.session_id {
        return Err(BridgeError::ProtocolIncompatible);
    }
    if envelope
        .correlation_id
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 128)
    {
        return Err(BridgeError::ReplayDetected);
    }
    let sent_at = OffsetDateTime::parse(&envelope.sent_at, &Rfc3339)
        .map_err(|_| BridgeError::TransferRejected("invalid sentAt timestamp".to_owned()))?;
    let skew_seconds = (OffsetDateTime::now_utc() - sent_at)
        .whole_seconds()
        .unsigned_abs();
    if skew_seconds > 300 {
        tracing::warn!(session_id = %runtime.session_id, skew_seconds, "Studio bridge clock skew exceeds five minutes");
    }
    match runtime.incoming_sequence.check(&envelope, raw)? {
        SequenceDecision::Duplicate => {
            return Ok(Some((
                "protocol.duplicate-ack".to_owned(),
                json!({"messageId": envelope.message_id}),
                envelope.correlation_id,
            )));
        }
        SequenceDecision::New => {}
    }
    let correlation = envelope
        .correlation_id
        .clone()
        .or(Some(envelope.message_id.clone()));
    match envelope.kind.as_str() {
        "session.hello" => {
            let hello = parse_hello(&envelope.payload)?;
            let binding = state.binding.read().await.clone();
            let compatibility = binding
                .as_ref()
                .map(|candidate| hello_matches_binding(&hello, candidate))
                .unwrap_or(false);
            runtime.hello = Some(hello.clone());
            if let Some(session) = state.sessions.lock().await.get_mut(&runtime.session_id) {
                session.hello = Some(hello);
                session.bound_context_version = compatibility
                    .then(|| binding.as_ref().map(|value| value.context_version))
                    .flatten();
            }
            Ok(Some((
                "session.context".to_owned(),
                context_payload(
                    binding,
                    compatibility,
                    &state.config.limits,
                    state.config.heartbeat_interval,
                ),
                correlation,
            )))
        }
        "session.pong" => Ok(None),
        "session.goodbye" => Ok(None),
        "studio.selection-changed"
        | "studio.run-state-changed"
        | "studio.place-context-changed"
        | "art.cell-dirty-changed"
        | "art.lock-status-changed"
        | "diagnostic.event" => {
            let _ = state.events.send(BridgeEvent::StudioEvent {
                session_id: runtime.session_id.clone(),
                kind: envelope.kind,
                payload: envelope.payload,
            });
            Ok(None)
        }
        "transfer.offer" => {
            let binding = require_bound_context(state, runtime, &envelope.payload).await?;
            let offer: TransferOffer = serde_json::from_value(envelope.payload)?;
            let accepted = runtime.transfer.offer(offer, binding)?;
            Ok(Some(("transfer.accept".to_owned(), accepted, correlation)))
        }
        "transfer.chunk" => {
            let binding = require_bound_context(state, runtime, &envelope.payload).await?;
            let chunk: TransferChunk = serde_json::from_value(envelope.payload)?;
            runtime
                .transfer
                .require_context(&chunk.transfer_id, &binding)?;
            let ack = runtime.transfer.chunk(chunk)?;
            Ok(Some(("transfer.ack".to_owned(), ack, correlation)))
        }
        "transfer.complete" => {
            let binding = require_bound_context(state, runtime, &envelope.payload).await?;
            let transfer_id = envelope
                .payload
                .get("transferId")
                .and_then(Value::as_str)
                .ok_or_else(|| BridgeError::TransferRejected("missing transferId".to_owned()))?;
            runtime.transfer.require_context(transfer_id, &binding)?;
            let verified = runtime.transfer.complete(transfer_id)?;
            if let Some(orchestrator) = state.orchestrator.read().await.clone() {
                orchestrator
                    .transfer_verified(VerifiedTransferInput {
                        session_id: runtime.session_id.clone(),
                        transfer_id: verified.transfer_id.clone(),
                        content_hash: verified.content_hash.clone(),
                        path: verified.path.clone(),
                        size_bytes: verified.size_bytes,
                    })
                    .await
                    .map_err(BridgeError::OrchestrationFailed)?;
            }
            let _ = state.events.send(BridgeEvent::TransferVerified {
                session_id: runtime.session_id.clone(),
                transfer_id: verified.transfer_id.clone(),
                content_hash: verified.content_hash.clone(),
                path: verified.path.clone(),
                size_bytes: verified.size_bytes,
            });
            Ok(Some((
                "transfer.verified".to_owned(),
                json!({
                    "transferId": verified.transfer_id,
                    "contentHash": verified.content_hash,
                    "sizeBytes": verified.size_bytes,
                }),
                correlation,
            )))
        }
        "transfer.cancel" => {
            if let Some(id) = envelope.payload.get("transferId").and_then(Value::as_str) {
                runtime.transfer.cancel(id);
            }
            Ok(None)
        }
        "art.capture.proposal" => {
            let binding = require_bound_context(state, runtime, &envelope.payload).await?;
            let cell_id = required_string(&envelope.payload, "cellId")?;
            let transfer_id = required_string(&envelope.payload, "transferId")?;
            let slot_path = required_string(&envelope.payload, "slotPath")?;
            if !slot_path.starts_with('/') || slot_path.len() > 1024 {
                return Err(BridgeError::TransferRejected(
                    "invalid managed DataModel slot".to_owned(),
                ));
            }
            if !runtime
                .transfer
                .is_verified_for(&transfer_id, &cell_id, &binding)
            {
                return Err(BridgeError::TransferRejected(
                    "capture payload is not verified".to_owned(),
                ));
            }
            let epoch = envelope
                .payload
                .get("mutationEpoch")
                .and_then(Value::as_u64)
                .ok_or_else(|| BridgeError::TransferRejected("missing mutationEpoch".to_owned()))?;
            let base = envelope
                .payload
                .get("baseRevision")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let hello = runtime.hello.as_ref().ok_or(BridgeError::ContextStale)?;
            let commit = if let Some(orchestrator) = state.orchestrator.read().await.clone() {
                orchestrator
                    .capture_proposal(CaptureProposalInput {
                        session_id: runtime.session_id.clone(),
                        plugin_installation_id: state
                            .sessions
                            .lock()
                            .await
                            .get(&runtime.session_id)
                            .map(|session| session.plugin_installation_id.clone())
                            .ok_or(BridgeError::ContextStale)?,
                        cell_id: cell_id.clone(),
                        transfer_id: transfer_id.clone(),
                        slot_path: slot_path.clone(),
                        mutation_epoch: epoch,
                        base_revision: base.clone(),
                        studio_user_id: hello.studio_user_id.clone(),
                        studio_version: hello.studio_version.clone(),
                    })
                    .await
                    .map_err(BridgeError::OrchestrationFailed)?
            } else {
                CaptureProposalCommit {
                    revision_id: String::new(),
                    status: "queued".to_owned(),
                    state_root: String::new(),
                    fanout: None,
                }
            };
            let _ = state.events.send(BridgeEvent::CaptureProposal {
                session_id: runtime.session_id.clone(),
                cell_id: cell_id.clone(),
                transfer_id: transfer_id.clone(),
                slot_path,
                mutation_epoch: epoch,
                base_revision: base,
            });
            let mut fanned_out = 0;
            if let Some(revision) = commit.fanout.clone() {
                validate_incoming_revision(state, &revision)?;
                fanned_out = fanout_accepted_revision(state, &runtime.session_id, revision).await?;
                if let Some(binding) = state.binding.write().await.as_mut() {
                    binding.accepted_art_head = Some(commit.revision_id.clone());
                    binding.accepted_state_root = Some(commit.state_root.clone());
                }
            }
            Ok(Some((
                "art.capture.accepted".to_owned(),
                json!({
                    "accepted": true,
                    "durable": !commit.revision_id.is_empty(),
                    "revisionId": if commit.revision_id.is_empty() { Value::Null } else { json!(commit.revision_id) },
                    "status": commit.status,
                    "stateRoot": if commit.state_root.is_empty() { Value::Null } else { json!(commit.state_root) },
                    "fanoutSessions": fanned_out,
                }),
                correlation,
            )))
        }
        "art.apply.receipt" | "command.succeeded" | "command.failed" | "command.cancelled" => {
            require_bound_context(state, runtime, &envelope.payload).await?;
            let command_id = required_string(&envelope.payload, "commandId")?;
            if let Some(previous) = runtime.command_receipts.get(&command_id) {
                if previous != &envelope.payload {
                    return Err(BridgeError::ReplayDetected);
                }
                return Ok(Some((
                    "protocol.duplicate-ack".to_owned(),
                    json!({"commandId": command_id}),
                    correlation,
                )));
            }
            let revision_id = required_string(&envelope.payload, "revisionId")?;
            let status = envelope
                .payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(&envelope.kind)
                .to_owned();
            let verification = envelope
                .payload
                .get("verification")
                .and_then(Value::as_str)
                .unwrap_or("applied-unverified")
                .to_owned();
            runtime
                .command_receipts
                .insert(command_id.clone(), envelope.payload);
            let _ = state.events.send(BridgeEvent::ApplyReceipt {
                session_id: runtime.session_id.clone(),
                command_id,
                revision_id,
                status,
                verification,
            });
            Ok(None)
        }
        "command.request" => {
            require_bound_context(state, runtime, &envelope.payload).await?;
            let command_id = required_string(&envelope.payload, "commandId")?;
            validate_scoped_id(&command_id, "op_")?;
            let command = required_string(&envelope.payload, "command")?;
            if command != "art.register-cell" {
                return Err(BridgeError::CommandUnsupported(command));
            }
            let arguments = envelope
                .payload
                .get("arguments")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    BridgeError::TransferRejected("missing command arguments".to_owned())
                })?;
            let cell_id = arguments
                .get("cellId")
                .and_then(Value::as_str)
                .ok_or_else(|| BridgeError::TransferRejected("missing cellId".to_owned()))?;
            validate_scoped_id(cell_id, "cell_")?;
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let class_name = arguments
                .get("className")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if path.is_empty()
                || path.len() > 1024
                || class_name.is_empty()
                || class_name.len() > 128
            {
                return Err(BridgeError::TransferRejected(
                    "invalid cell registration intent".to_owned(),
                ));
            }
            let _ = state.events.send(BridgeEvent::StudioEvent {
                session_id: runtime.session_id.clone(),
                kind: "art.register-cell".to_owned(),
                payload: envelope.payload.clone(),
            });
            Ok(Some((
                "command.succeeded".to_owned(),
                json!({
                    "contextVersion": state.binding.read().await.as_ref().map(|value| value.context_version),
                    "commandId": command_id,
                    "command": "art.register-cell",
                    "cellId": cell_id,
                    "approved": true,
                }),
                correlation,
            )))
        }
        "command.accepted" | "command.progress" => {
            require_bound_context(state, runtime, &envelope.payload).await?;
            let _ = state.events.send(BridgeEvent::StudioEvent {
                session_id: runtime.session_id.clone(),
                kind: envelope.kind,
                payload: envelope.payload,
            });
            Ok(None)
        }
        _ => Err(BridgeError::CommandUnsupported(envelope.kind)),
    }
}

fn required_string(value: &Value, key: &str) -> Result<String, BridgeError> {
    let result = value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| BridgeError::TransferRejected(format!("missing or invalid {key}")))?;
    Ok(result.to_owned())
}

fn parse_hello(payload: &Value) -> Result<HelloContext, BridgeError> {
    let mut hello = HelloContext {
        universe_id: payload
            .get("universeId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        place_id: payload
            .get("placeId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        project_id: payload
            .get("projectId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        channel: payload
            .get("channel")
            .and_then(Value::as_str)
            .map(str::to_owned),
        studio_user_id: payload
            .get("studioUserId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        studio_version: payload
            .get("studioVersion")
            .and_then(Value::as_str)
            .map(str::to_owned),
        capabilities: HashSet::new(),
    };
    if let Some(capabilities) = payload.get("capabilities").and_then(Value::as_array) {
        for capability in capabilities.iter().take(64) {
            if let Some(capability) = capability.as_str().filter(|value| value.len() <= 64) {
                hello.capabilities.insert(capability.to_owned());
            }
        }
    }
    if !hello.capabilities.contains("base64-chunks") {
        return Err(BridgeError::ProtocolIncompatible);
    }
    Ok(hello)
}

fn hello_matches_binding(hello: &HelloContext, binding: &SessionBinding) -> bool {
    hello.project_id.as_deref() == Some(binding.project_id.as_str())
        && hello.channel.as_deref() == Some(binding.channel.as_str())
        && match (&binding.place_id, &hello.place_id) {
            (Some(expected), Some(actual)) => expected == actual,
            (None, _) => true,
            _ => false,
        }
        && match (&binding.universe_id, &hello.universe_id) {
            (Some(expected), Some(actual)) => expected == actual,
            (None, _) => true,
            _ => false,
        }
}

fn context_payload(
    binding: Option<SessionBinding>,
    compatible: bool,
    limits: &BridgeLimits,
    heartbeat_interval: Duration,
) -> Value {
    let allowed = if compatible {
        json!([
            "art.capture",
            "art.apply",
            "art.register-cell",
            "validation.run",
            "session.refresh-context"
        ])
    } else {
        json!([])
    };
    json!({
        "bound": compatible,
        "unboundReason": if compatible { Value::Null } else { json!("identity-mismatch-or-no-binding") },
        "binding": binding,
        "allowedCommands": allowed,
        "heartbeatSeconds": heartbeat_interval.as_secs(),
        "limits": limits,
        "capabilities": ["base64-chunks", "semantic-fingerprint-v1", "apply-transaction-v1"],
    })
}

async fn require_bound_context(
    state: &Arc<BridgeState>,
    runtime: &SessionRuntime,
    payload: &Value,
) -> Result<SessionBinding, BridgeError> {
    let binding = state.binding.read().await;
    let binding = binding.as_ref().ok_or(BridgeError::ContextStale)?;
    let hello = runtime.hello.as_ref().ok_or(BridgeError::ContextStale)?;
    if !hello_matches_binding(hello, binding) {
        return Err(BridgeError::ContextStale);
    }
    let received = payload
        .get("contextVersion")
        .and_then(Value::as_u64)
        .ok_or(BridgeError::ContextStale)?;
    if received != binding.context_version {
        return Err(BridgeError::ContextStale);
    }
    Ok(binding.clone())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferOffer {
    context_version: u64,
    transfer_id: String,
    direction: String,
    purpose: String,
    resource_id: String,
    content_hash: String,
    media_type: String,
    size_bytes: u64,
    chunk_size_bytes: usize,
    chunk_count: usize,
    encoding: String,
    compression: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferChunk {
    context_version: u64,
    transfer_id: String,
    chunk_index: usize,
    offset_bytes: u64,
    raw_size_bytes: usize,
    chunk_hash: String,
    data: String,
}

#[derive(Debug)]
struct UploadState {
    offer: TransferOffer,
    binding: SessionBinding,
    path: PathBuf,
    received: Vec<bool>,
    received_hashes: Vec<Option<String>>,
}

#[derive(Clone, Debug)]
struct VerifiedTransfer {
    transfer_id: String,
    content_hash: String,
    path: PathBuf,
    size_bytes: u64,
    resource_id: String,
    binding: SessionBinding,
}

#[derive(Debug)]
struct TransferManager {
    directory: PathBuf,
    limits: BridgeLimits,
    session_id: String,
    upload: Option<UploadState>,
    verified: HashMap<String, VerifiedTransfer>,
}

impl TransferManager {
    fn new(directory: PathBuf, limits: BridgeLimits, session_id: String) -> Self {
        Self {
            directory,
            limits,
            session_id,
            upload: None,
            verified: HashMap::new(),
        }
    }

    fn offer(
        &mut self,
        offer: TransferOffer,
        binding: SessionBinding,
    ) -> Result<Value, BridgeError> {
        if self.upload.is_some() {
            return Err(BridgeError::TransferRejected(
                "one upload already active".to_owned(),
            ));
        }
        validate_scoped_id(&offer.transfer_id, "op_")?;
        validate_scoped_id(&offer.resource_id, "cell_")?;
        if offer.context_version != binding.context_version {
            return Err(BridgeError::ContextStale);
        }
        if offer.direction != "upload"
            || offer.purpose != "art-cell"
            || offer.media_type != "application/x-roblox-rbxm"
            || offer.encoding != "base64url"
            || offer.compression != "none"
        {
            return Err(BridgeError::TransferRejected(
                "unsupported transfer profile".to_owned(),
            ));
        }
        if offer.size_bytes == 0 || offer.size_bytes > self.limits.max_transfer_bytes {
            return Err(BridgeError::TransferRejected(
                "content size outside limits".to_owned(),
            ));
        }
        if offer.chunk_size_bytes == 0 || offer.chunk_size_bytes > self.limits.max_chunk_bytes {
            return Err(BridgeError::TransferRejected(
                "chunk size outside limits".to_owned(),
            ));
        }
        let expected_chunks = offer.size_bytes.div_ceil(offer.chunk_size_bytes as u64) as usize;
        if offer.chunk_count != expected_chunks || offer.chunk_count > 4096 {
            return Err(BridgeError::TransferRejected(
                "invalid chunk count".to_owned(),
            ));
        }
        validate_blake3_hash(&offer.content_hash)?;
        let path = self
            .directory
            .join(format!("{}-{}.partial", self.session_id, offer.transfer_id));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&path)?;
        file.set_len(offer.size_bytes)?;
        let count = offer.chunk_count;
        let transfer_id = offer.transfer_id.clone();
        self.upload = Some(UploadState {
            offer,
            binding,
            path,
            received: vec![false; count],
            received_hashes: vec![None; count],
        });
        Ok(json!({"transferId": transfer_id, "windowSize": 4, "received": []}))
    }

    fn chunk(&mut self, chunk: TransferChunk) -> Result<Value, BridgeError> {
        let upload = self
            .upload
            .as_mut()
            .ok_or_else(|| BridgeError::TransferRejected("no active upload".to_owned()))?;
        if chunk.transfer_id != upload.offer.transfer_id
            || chunk.context_version != upload.binding.context_version
            || chunk.chunk_index >= upload.offer.chunk_count
        {
            return Err(BridgeError::TransferRejected(
                "chunk does not belong to upload".to_owned(),
            ));
        }
        let expected_offset = (chunk.chunk_index * upload.offer.chunk_size_bytes) as u64;
        let expected_size = ((upload.offer.size_bytes - expected_offset)
            .min(upload.offer.chunk_size_bytes as u64)) as usize;
        if chunk.offset_bytes != expected_offset
            || chunk.raw_size_bytes != expected_size
            || chunk.data.len() > self.limits.max_chunk_bytes.saturating_mul(4).div_ceil(3) + 8
        {
            return Err(BridgeError::TransferRejected(
                "invalid chunk geometry".to_owned(),
            ));
        }
        validate_blake3_hash(&chunk.chunk_hash)?;
        let decoded = URL_SAFE_NO_PAD
            .decode(chunk.data.as_bytes())
            .map_err(|_| BridgeError::TransferRejected("invalid base64url".to_owned()))?;
        if decoded.len() != expected_size {
            return Err(BridgeError::TransferRejected(
                "decoded chunk size mismatch".to_owned(),
            ));
        }
        if blake3_label(&decoded) != chunk.chunk_hash {
            return Err(BridgeError::ChunkHashMismatch);
        }
        if upload.received[chunk.chunk_index] {
            if upload.received_hashes[chunk.chunk_index].as_deref() != Some(&chunk.chunk_hash) {
                return Err(BridgeError::ReplayDetected);
            }
        } else {
            let mut file = OpenOptions::new().write(true).open(&upload.path)?;
            file.seek(SeekFrom::Start(expected_offset))?;
            file.write_all(&decoded)?;
            file.sync_data()?;
            upload.received[chunk.chunk_index] = true;
            upload.received_hashes[chunk.chunk_index] = Some(chunk.chunk_hash);
        }
        let highest = upload
            .received
            .iter()
            .take_while(|received| **received)
            .count();
        let missing: Vec<usize> = (0..upload.offer.chunk_count)
            .filter(|index| !upload.received[*index])
            .take(64)
            .collect();
        Ok(json!({
            "transferId": chunk.transfer_id,
            "highestContiguous": if highest == 0 { Value::Null } else { json!(highest - 1) },
            "missing": missing,
        }))
    }

    fn complete(&mut self, transfer_id: &str) -> Result<VerifiedTransfer, BridgeError> {
        if let Some(verified) = self.verified.get(transfer_id) {
            return Ok(verified.clone());
        }
        let upload = self
            .upload
            .take()
            .ok_or_else(|| BridgeError::TransferRejected("no active upload".to_owned()))?;
        if transfer_id != upload.offer.transfer_id
            || upload.received.iter().any(|received| !received)
        {
            self.upload = Some(upload);
            return Err(BridgeError::TransferRejected(
                "upload is incomplete".to_owned(),
            ));
        }
        let mut file = File::open(&upload.path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual_hash = format!("b3-256:{}", base32_lower(hasher.finalize().as_bytes()));
        if actual_hash != upload.offer.content_hash {
            let _ = fs::remove_file(&upload.path);
            return Err(BridgeError::ContentHashMismatch);
        }
        let final_path = self
            .directory
            .join(format!("{}-{}.verified", self.session_id, transfer_id));
        fs::rename(&upload.path, &final_path)?;
        let verified = VerifiedTransfer {
            transfer_id: transfer_id.to_owned(),
            content_hash: actual_hash,
            path: final_path,
            size_bytes: upload.offer.size_bytes,
            resource_id: upload.offer.resource_id,
            binding: upload.binding,
        };
        self.verified
            .insert(transfer_id.to_owned(), verified.clone());
        Ok(verified)
    }

    fn require_context(
        &self,
        transfer_id: &str,
        binding: &SessionBinding,
    ) -> Result<(), BridgeError> {
        let expected = self
            .upload
            .as_ref()
            .filter(|upload| upload.offer.transfer_id == transfer_id)
            .map(|upload| &upload.binding)
            .or_else(|| {
                self.verified
                    .get(transfer_id)
                    .map(|verified| &verified.binding)
            })
            .ok_or_else(|| BridgeError::TransferRejected("unknown transfer".to_owned()))?;
        if expected != binding {
            return Err(BridgeError::ContextStale);
        }
        Ok(())
    }

    fn is_verified_for(
        &self,
        transfer_id: &str,
        resource_id: &str,
        binding: &SessionBinding,
    ) -> bool {
        self.verified.get(transfer_id).is_some_and(|verified| {
            verified.resource_id == resource_id && &verified.binding == binding
        })
    }

    fn cancel(&mut self, transfer_id: &str) {
        if self
            .upload
            .as_ref()
            .is_some_and(|upload| upload.offer.transfer_id == transfer_id)
            && let Some(upload) = self.upload.take()
        {
            let _ = fs::remove_file(upload.path);
        }
    }

    fn cancel_all(&mut self) {
        if let Some(upload) = self.upload.take() {
            let _ = fs::remove_file(upload.path);
        }
    }
}

fn validate_blake3_hash(hash: &str) -> Result<(), BridgeError> {
    let Some(base32) = hash.strip_prefix("b3-256:") else {
        return Err(BridgeError::TransferRejected(
            "unsupported content hash".to_owned(),
        ));
    };
    if base32.len() != 52
        || !base32
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'))
    {
        return Err(BridgeError::TransferRejected(
            "invalid content hash".to_owned(),
        ));
    }
    Ok(())
}

fn blake3_label(data: &[u8]) -> String {
    format!("b3-256:{}", base32_lower(blake3::hash(data).as_bytes()))
}

fn base32_lower(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut output = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut accumulator = 0_u16;
    let mut bits = 0_u8;
    for byte in data {
        accumulator = (accumulator << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = usize::from((accumulator >> bits) & 0x1f);
            output.push(char::from(ALPHABET[index]));
        }
        if bits == 0 {
            accumulator = 0;
        } else {
            accumulator &= (1_u16 << bits) - 1;
        }
    }
    if bits > 0 {
        let index = usize::from((accumulator << (5 - bits)) & 0x1f);
        output.push(char::from(ALPHABET[index]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_binding(context_version: u64) -> SessionBinding {
        SessionBinding {
            project_id: "prj_test".into(),
            workspace_id: "wsp_test".into(),
            universe_id: Some("11".into()),
            place_id: Some("22".into()),
            channel: "art-main".into(),
            config_hash: blake3_label(b"config"),
            ownership_hash: blake3_label(b"ownership"),
            policy_hash: blake3_label(b"policy"),
            accepted_art_head: None,
            accepted_state_root: None,
            context_version,
        }
    }

    fn matching_hello() -> HelloContext {
        HelloContext {
            project_id: Some("prj_test".into()),
            channel: Some("art-main".into()),
            universe_id: Some("11".into()),
            place_id: Some("22".into()),
            ..HelloContext::default()
        }
    }

    fn test_state(directory: PathBuf) -> Arc<BridgeState> {
        let (events, _) = broadcast::channel(32);
        Arc::new(BridgeState {
            config: BridgeConfig {
                transfer_dir: directory,
                ..BridgeConfig::default()
            },
            installation_id: format!("ins_{}", Uuid::new_v4()),
            web_socket_url: RwLock::new("ws://127.0.0.1:40000/v1/studio".to_owned()),
            pairing: Mutex::new(PairingRegistry::default()),
            binding: RwLock::new(None),
            orchestrator: RwLock::new(None),
            downloads: Mutex::new(HashMap::new()),
            events,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    #[test]
    fn host_validation_rejects_names_and_non_loopback() {
        for invalid in [
            "localhost:34873",
            "example.com:34873",
            "192.168.1.2:34873",
            "127.0.0.1:80",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, invalid.parse().unwrap());
            assert!(
                validate_host_header(&headers, 34_873).is_err(),
                "accepted {invalid}"
            );
        }
        for valid in ["127.0.0.1:34873", "[::1]:34873"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, valid.parse().unwrap());
            assert!(
                validate_host_header(&headers, 34_873).is_ok(),
                "rejected {valid}"
            );
        }
    }

    #[test]
    fn protocol_negotiation_is_major_safe() {
        assert_eq!(negotiate_protocol("1.0", "1.0").unwrap(), "1.0");
        assert!(negotiate_protocol("1.1", "1.4").is_err());
        assert!(negotiate_protocol("0.9", "2.0").is_err());
        assert!(negotiate_protocol("garbage", "1.0").is_err());
    }

    #[test]
    fn blake3_labels_match_cross_language_cas_vectors() {
        assert_eq!(
            blake3_label(b""),
            "b3-256:v4jutopv7gq2nicajxvdnxgjjgn4wjojvxarfn6mtkj4vza7gjra"
        );
        assert_eq!(
            blake3_label(b"abc"),
            "b3-256:mq33hlbyizith75whn2soounwvemkwcglv45wa75gwogzvn5twcq"
        );
    }

    #[test]
    fn sequencing_detects_gap_and_changed_replay() {
        let mut guard = SequenceGuard::new();
        let envelope = Envelope {
            protocol_version: "1.0".to_owned(),
            kind: "session.pong".to_owned(),
            message_id: "msg_first".to_owned(),
            correlation_id: None,
            sequence: 1,
            sent_at: "2026-01-01T00:00:00Z".to_owned(),
            session_id: "ses_test".to_owned(),
            payload: json!({}),
        };
        let raw = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(guard.check(&envelope, &raw).unwrap(), SequenceDecision::New);
        assert_eq!(
            guard.check(&envelope, &raw).unwrap(),
            SequenceDecision::Duplicate
        );
        let mut changed = envelope.clone();
        changed.payload = json!({"changed": true});
        let changed_raw = serde_json::to_vec(&changed).unwrap();
        assert!(matches!(
            guard.check(&changed, &changed_raw),
            Err(BridgeError::ReplayDetected)
        ));
        let mut gap = envelope;
        gap.message_id = "msg_gap".to_owned();
        gap.sequence = 3;
        assert!(matches!(
            guard.check(&gap, b"gap"),
            Err(BridgeError::SequenceGap {
                expected: 2,
                received: 3
            })
        ));
    }

    #[tokio::test]
    async fn pairing_requires_exact_approval_and_credential_is_scoped() {
        let state = test_state(std::env::temp_dir());
        let (challenge, _) = issue_challenge(&state).await;
        let code = state.pairing.lock().await.challenges[&challenge]
            .pairing_code
            .clone();
        let request = || PairRequest {
            challenge: challenge.clone(),
            pairing_code: code.clone(),
            plugin_installation_id: "plg_12345678".to_owned(),
            plugin_version: "0.1.0".to_owned(),
            protocol_version: "1.0".to_owned(),
            studio: StudioIdentity::default(),
        };
        assert!(matches!(
            complete_pairing(&state, request()).await,
            Err(BridgeError::PairingRequired)
        ));
        BridgeHandle {
            state: state.clone(),
        }
        .approve_pairing(&challenge, "plg_12345678")
        .await
        .unwrap();
        let success = complete_pairing(&state, request()).await.unwrap();
        authenticate_credential(&state, "plg_12345678", &success.credential)
            .await
            .unwrap();
        assert!(
            authenticate_credential(&state, "plg_other1234", &success.credential)
                .await
                .is_err()
        );
    }

    #[test]
    fn transfer_is_bounded_hash_checked_and_quarantined() {
        let directory = std::env::temp_dir().join(format!("neuman-bridge-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let content = b"a deterministic native fixture";
        let mut manager = TransferManager::new(
            directory.clone(),
            BridgeLimits::default(),
            "ses_test1234".to_owned(),
        );
        let offer = TransferOffer {
            context_version: 7,
            transfer_id: "op_transfer1234".to_owned(),
            direction: "upload".to_owned(),
            purpose: "art-cell".to_owned(),
            resource_id: "cell_fixture1234".to_owned(),
            content_hash: blake3_label(content),
            media_type: "application/x-roblox-rbxm".to_owned(),
            size_bytes: content.len() as u64,
            chunk_size_bytes: 256 * 1024,
            chunk_count: 1,
            encoding: "base64url".to_owned(),
            compression: "none".to_owned(),
        };
        let binding = test_binding(7);
        manager.offer(offer, binding.clone()).unwrap();
        manager
            .chunk(TransferChunk {
                context_version: 7,
                transfer_id: "op_transfer1234".to_owned(),
                chunk_index: 0,
                offset_bytes: 0,
                raw_size_bytes: content.len(),
                chunk_hash: blake3_label(content),
                data: URL_SAFE_NO_PAD.encode(content),
            })
            .unwrap();
        let verified = manager.complete("op_transfer1234").unwrap();
        assert!(verified.path.exists());
        assert_eq!(fs::read(&verified.path).unwrap(), content);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn transfer_is_bound_to_resource_and_workspace_generation() {
        let directory =
            std::env::temp_dir().join(format!("neuman-bridge-context-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let content = b"context-bound native fixture";
        let mut manager = TransferManager::new(
            directory.clone(),
            BridgeLimits::default(),
            "ses_context1234".to_owned(),
        );
        let binding = test_binding(11);
        manager
            .offer(
                TransferOffer {
                    context_version: 11,
                    transfer_id: "op_context1234".into(),
                    direction: "upload".into(),
                    purpose: "art-cell".into(),
                    resource_id: "cell_context1234".into(),
                    content_hash: blake3_label(content),
                    media_type: "application/x-roblox-rbxm".into(),
                    size_bytes: content.len() as u64,
                    chunk_size_bytes: 256 * 1024,
                    chunk_count: 1,
                    encoding: "base64url".into(),
                    compression: "none".into(),
                },
                binding.clone(),
            )
            .unwrap();
        let mut switched = binding.clone();
        switched.workspace_id = "wsp_other".into();
        switched.context_version = 12;
        assert!(matches!(
            manager.require_context("op_context1234", &switched),
            Err(BridgeError::ContextStale)
        ));
        manager
            .chunk(TransferChunk {
                context_version: 11,
                transfer_id: "op_context1234".into(),
                chunk_index: 0,
                offset_bytes: 0,
                raw_size_bytes: content.len(),
                chunk_hash: blake3_label(content),
                data: URL_SAFE_NO_PAD.encode(content),
            })
            .unwrap();
        manager.complete("op_context1234").unwrap();
        assert!(manager.is_verified_for("op_context1234", "cell_context1234", &binding));
        assert!(!manager.is_verified_for("op_context1234", "cell_other1234", &binding));
        assert!(!manager.is_verified_for("op_context1234", "cell_context1234", &switched));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn non_loopback_listener_is_rejected_before_bind() {
        let config = BridgeConfig {
            discovery_addr: "0.0.0.0:0".parse().unwrap(),
            session_addr: "127.0.0.1:0".parse().unwrap(),
            ..BridgeConfig::default()
        };
        assert!(matches!(
            BridgeService::start(config).await,
            Err(BridgeError::DiscoveryInvalid(_))
        ));
    }

    #[tokio::test]
    async fn discovery_serves_only_the_minimal_no_store_document() {
        let directory =
            std::env::temp_dir().join(format!("neuman-bridge-discovery-{}", Uuid::new_v4()));
        let running = BridgeService::start(BridgeConfig {
            discovery_addr: "127.0.0.1:0".parse().unwrap(),
            session_addr: "127.0.0.1:0".parse().unwrap(),
            transfer_dir: directory.clone(),
            ..BridgeConfig::default()
        })
        .await
        .unwrap();
        let response = reqwest::Client::new()
            .get(format!(
                "http://{}{}",
                running.discovery_addr, DISCOVERY_PATH
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let document: Value = response.json().await.unwrap();
        assert_eq!(document["schemaVersion"], PROTOCOL_VERSION);
        assert!(
            document["webSocketUrl"]
                .as_str()
                .unwrap()
                .starts_with("ws://127.0.0.1:")
        );
        assert!(document.get("projectId").is_none());
        assert!(document.get("accountId").is_none());
        running.shutdown().await;
        fs::remove_dir_all(directory).unwrap();
    }

    fn incoming_fixture(bytes: &[u8]) -> IncomingRevision {
        let cell_id = format!("cell_{}", Uuid::new_v4());
        IncomingRevision {
            revision_id: format!("art_{}", Uuid::now_v7()),
            state_root: blake3_label(b"state"),
            cell_ids: vec![cell_id.clone()],
            author_display: "artist".into(),
            summary: "Accepted art".into(),
            cells: vec![IncomingCell {
                cell_id,
                parent_path: "/Workspace/Art".into(),
                content_hash: blake3_label(bytes),
                size_bytes: bytes.len() as u64,
                data: Some(URL_SAFE_NO_PAD.encode(bytes)),
                download: None,
            }],
        }
    }

    #[tokio::test]
    async fn large_apply_uses_session_scoped_hash_verified_download() {
        let directory =
            std::env::temp_dir().join(format!("neuman-bridge-download-{}", Uuid::new_v4()));
        let running = BridgeService::start(BridgeConfig {
            discovery_addr: "127.0.0.1:0".parse().unwrap(),
            session_addr: "127.0.0.1:0".parse().unwrap(),
            transfer_dir: directory.clone(),
            ..BridgeConfig::default()
        })
        .await
        .unwrap();
        let session_id = format!("ses_{}", Uuid::new_v4());
        let mut capabilities = HashSet::new();
        capabilities.insert("http-download-v1".to_owned());
        let runtime = SessionRuntime {
            session_id: session_id.clone(),
            incoming_sequence: SequenceGuard::new(),
            outgoing_sequence: 1,
            transfer: TransferManager::new(
                directory.clone(),
                BridgeLimits::default(),
                session_id.clone(),
            ),
            hello: Some(HelloContext {
                capabilities,
                ..HelloContext::default()
            }),
            command_receipts: HashMap::new(),
        };
        let bytes = vec![0x5a; 800_000];
        let prepared =
            prepare_revision_for_session(&running.handle.state, &runtime, incoming_fixture(&bytes))
                .await
                .unwrap();
        assert!(prepared.cells[0].data.is_none());
        let download = prepared.cells[0].download.as_ref().unwrap();
        let denied = reqwest::Client::new()
            .get(&download.url)
            .header(
                "authorization",
                format!("NeuManTransfer {}", download.token),
            )
            .header("x-neuman-session", "ses_wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let response = reqwest::Client::new()
            .get(&download.url)
            .header(
                "authorization",
                format!("NeuManTransfer {}", download.token),
            )
            .header("x-neuman-session", &session_id)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["x-neuman-content-hash"],
            blake3_label(&bytes)
        );
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
        running.shutdown().await;
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn accepted_fanout_excludes_source_and_queues_one_atomic_push() {
        let state = test_state(std::env::temp_dir());
        *state.binding.write().await = Some(test_binding(4));
        let (source_tx, mut source_rx) = mpsc::channel(2);
        let (developer_tx, mut developer_rx) = mpsc::channel(2);
        state.sessions.lock().await.extend([
            (
                "ses_source".into(),
                SessionSender {
                    plugin_installation_id: "plg_source".into(),
                    hello: Some(matching_hello()),
                    bound_context_version: Some(4),
                    tx: source_tx,
                },
            ),
            (
                "ses_developer".into(),
                SessionSender {
                    plugin_installation_id: "plg_developer".into(),
                    hello: Some(matching_hello()),
                    bound_context_version: Some(4),
                    tx: developer_tx,
                },
            ),
        ]);
        let revision = incoming_fixture(b"small cell");
        validate_incoming_revision(&state, &revision).unwrap();
        assert_eq!(
            fanout_accepted_revision(&state, "ses_source", revision)
                .await
                .unwrap(),
            1
        );
        assert!(source_rx.try_recv().is_err());
        assert!(matches!(
            developer_rx.recv().await,
            Some(ServerPush::IncomingAndApply {
                context_version: 4,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn accepted_fanout_does_not_disclose_across_place_or_workspace_generation() {
        let state = test_state(std::env::temp_dir());
        *state.binding.write().await = Some(test_binding(9));
        let (current_tx, mut current_rx) = mpsc::channel(2);
        let (other_place_tx, mut other_place_rx) = mpsc::channel(2);
        let (stale_tx, mut stale_rx) = mpsc::channel(2);
        let mut other_place = matching_hello();
        other_place.place_id = Some("999".into());
        state.sessions.lock().await.extend([
            (
                "ses_current".into(),
                SessionSender {
                    plugin_installation_id: "plg_current".into(),
                    hello: Some(matching_hello()),
                    bound_context_version: Some(9),
                    tx: current_tx,
                },
            ),
            (
                "ses_other_place".into(),
                SessionSender {
                    plugin_installation_id: "plg_other_place".into(),
                    hello: Some(other_place),
                    bound_context_version: Some(9),
                    tx: other_place_tx,
                },
            ),
            (
                "ses_stale_workspace".into(),
                SessionSender {
                    plugin_installation_id: "plg_stale".into(),
                    hello: Some(matching_hello()),
                    bound_context_version: Some(8),
                    tx: stale_tx,
                },
            ),
        ]);
        assert_eq!(
            fanout_accepted_revision(&state, "", incoming_fixture(b"isolated"))
                .await
                .unwrap(),
            1
        );
        assert!(matches!(
            current_rx.recv().await,
            Some(ServerPush::IncomingAndApply { .. })
        ));
        assert!(other_place_rx.try_recv().is_err());
        assert!(stale_rx.try_recv().is_err());
    }
}
