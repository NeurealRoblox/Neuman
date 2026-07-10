//! Native NeuMan desktop shell and privileged command boundary.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use neuman::{
    bridge::{
        BridgeConfig, BridgeEvent, BridgeHandle, BridgeService, CaptureProposalCommit,
        CaptureProposalInput, IncomingCell, IncomingRevision, RunningBridge, SessionBinding,
        StudioBridgeOrchestrator, VerifiedTransferInput,
    },
    core::{LocalStudioOrchestrator, ProjectManifest, StudioCaptureRequest},
    domain::{
        ArtRevisionId, ArtRevisionStatus, CellId, ContentHash, ProjectId, WorkspaceId,
        hash_canonical,
    },
    git_rojo::{RojoServerState, RojoSessionKey, RojoSessionManager},
    hub_desktop::{HubCapture, HubDesktopAdapter, HubDesktopConfig},
    roblox_oauth::{
        OAuthSecret, ReqwestRobloxOAuthTransport, RobloxOAuthRefresher, RobloxPublicClientConfig,
        RobloxRefreshContext,
    },
    roblox_resources::{
        ReqwestRobloxResourceTransport, RobloxResourceInventory, RobloxResourceProvider,
        RobloxSelectionEvidence,
    },
    rojo_desktop_config::{RojoDesktopConfigAdapter, RojoDesktopSelection},
};
use rand::RngCore;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tauri::State;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    sync::{Mutex, watch},
};
use url::Url;

const OAUTH_ISSUER: &str = "https://apis.roblox.com/oauth/";
const OAUTH_DISCOVERY: &str = "https://apis.roblox.com/oauth/.well-known/openid-configuration";
const OAUTH_TOKEN_ENDPOINT: &str = "https://apis.roblox.com/oauth/v1/token";
const OAUTH_USERINFO_ENDPOINT: &str = "https://apis.roblox.com/oauth/v1/userinfo";
const OAUTH_JWKS_ENDPOINT: &str = "https://apis.roblox.com/oauth/v1/certs";
const OAUTH_REVOCATION_ENDPOINT: &str = "https://apis.roblox.com/oauth/v1/token/revoke";
const OAUTH_REDIRECT_URI: &str = "http://localhost:43891/oauth/callback";
const OAUTH_CALLBACK_PORT: u16 = 43_891;
const KEYRING_SERVICE: &str = "dev.neuman.manager";
const KEYRING_ACCOUNT: &str = "roblox-oauth";
const COMPILED_OAUTH_CLIENT_ID: Option<&str> = option_env!("NEUMAN_ROBLOX_OAUTH_CLIENT_ID");
const MAX_OAUTH_RESPONSE_BYTES: usize = 1024 * 1024;
const REQUIRED_OAUTH_SCOPES: [&str; 2] = ["openid", "profile"];

#[derive(Clone)]
struct DesktopState {
    oauth: Arc<Mutex<OAuthInternal>>,
    oauth_cancel: Arc<Mutex<Option<watch::Sender<bool>>>>,
    roblox_resources: Arc<Mutex<RobloxResourcePublicStatus>>,
    workspace: Arc<Mutex<Option<PathBuf>>>,
    /// Serializes workspace/binding swaps against every durable Studio mutation.
    bridge_context_gate: Arc<Mutex<()>>,
    bridge_runtime: Arc<Mutex<Option<RunningBridge>>>,
    bridge_handle: Arc<Mutex<Option<BridgeHandle>>>,
    bridge_ui: Arc<Mutex<BridgeUiInternal>>,
    hub_adapter: Arc<Mutex<Option<Arc<HubDesktopAdapter>>>>,
    hub_runtime: Arc<Mutex<Option<DesktopHubRuntime>>>,
    rojo_sessions: Arc<StdMutex<RojoSessionManager>>,
    rojo_adapter: Arc<RojoDesktopConfigAdapter>,
}

#[derive(Clone)]
struct DesktopStudioOrchestrator {
    state: DesktopState,
}

struct DesktopHubRuntime {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for DesktopStudioOrchestrator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopStudioOrchestrator")
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl StudioBridgeOrchestrator for DesktopStudioOrchestrator {
    async fn transfer_verified(&self, input: VerifiedTransferInput) -> Result<(), String> {
        let _context_guard = self.state.bridge_context_gate.lock().await;
        let workspace =
            self.state.workspace.lock().await.clone().ok_or_else(|| {
                "No validated workspace is bound to the Studio bridge.".to_owned()
            })?;
        tokio::task::spawn_blocking(move || {
            let orchestrator = LocalStudioOrchestrator::open(&workspace)
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
            let content_hash = input
                .content_hash
                .parse::<ContentHash>()
                .map_err(|error| format!("STUDIO_TRANSFER_HASH_INVALID: {error}"))?;
            orchestrator
                .ingest_verified_transfer(
                    &input.session_id,
                    &input.transfer_id,
                    content_hash,
                    input.size_bytes,
                    &input.path,
                )
                .map(|_| ())
                .map_err(|error| format!("{}: {}", error.code, error.message))
        })
        .await
        .map_err(|error| format!("Studio transfer task failed: {error}"))?
    }

    async fn capture_proposal(
        &self,
        input: CaptureProposalInput,
    ) -> Result<CaptureProposalCommit, String> {
        let _context_guard = self.state.bridge_context_gate.lock().await;
        let workspace =
            self.state.workspace.lock().await.clone().ok_or_else(|| {
                "No validated workspace is bound to the Studio bridge.".to_owned()
            })?;
        let (commit, hub_capture) = tokio::task::spawn_blocking(move || {
            let orchestrator = LocalStudioOrchestrator::open(&workspace)
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
            let base_state_root = orchestrator
                .accepted_head()
                .map_err(|error| format!("{}: {}", error.code, error.message))?
                .map(|revision| revision.state_root_hash);
            let source_session_id = input.session_id.clone();
            let cell_id = input
                .cell_id
                .parse::<CellId>()
                .map_err(|error| format!("STUDIO_CELL_ID_INVALID: {error}"))?;
            let base_revision = input
                .base_revision
                .as_deref()
                .map(str::parse::<ArtRevisionId>)
                .transpose()
                .map_err(|error| format!("STUDIO_BASE_INVALID: {error}"))?;
            let author = input
                .studio_user_id
                .as_deref()
                .map(|id| format!("roblox:{id}"))
                .unwrap_or_else(|| format!("studio-plugin:{}", input.plugin_installation_id));
            let message = format!(
                "Studio checkpoint {}{}",
                input.cell_id,
                input
                    .studio_version
                    .as_deref()
                    .map(|version| format!(" ({version})"))
                    .unwrap_or_default()
            );
            let outcome = orchestrator
                .commit_capture(StudioCaptureRequest {
                    session_id: input.session_id,
                    transfer_id: input.transfer_id,
                    cell_id,
                    slot_path: input.slot_path,
                    base_revision,
                    mutation_epoch: input.mutation_epoch,
                    author,
                    message,
                })
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
            let status = match outcome.revision.status {
                ArtRevisionStatus::Accepted => "accepted",
                ArtRevisionStatus::Proposed => "proposed",
                _ => "recorded",
            }
            .to_owned();
            let data = URL_SAFE_NO_PAD.encode(&outcome.changed_cell.bytes);
            let hub_capture = HubCapture {
                source_session_id,
                revision: outcome.revision.clone(),
                base_state_root,
                changed_cell_ids: vec![outcome.changed_cell.cell_id],
                cells: orchestrator
                    .materialize_revision_cells(&outcome.revision)
                    .map_err(|error| format!("{}: {}", error.code, error.message))?,
            };
            let fanout = if outcome.locally_accepted {
                Some(IncomingRevision {
                    revision_id: outcome.revision.art_revision_id.to_string(),
                    state_root: outcome.revision.state_root_hash.to_string(),
                    cell_ids: vec![outcome.changed_cell.cell_id.to_string()],
                    author_display: outcome.revision.author.clone(),
                    summary: outcome.revision.message.clone(),
                    cells: vec![IncomingCell {
                        cell_id: outcome.changed_cell.cell_id.to_string(),
                        parent_path: outcome.changed_cell.slot_path,
                        content_hash: outcome.changed_cell.content_hash.to_string(),
                        size_bytes: outcome.changed_cell.bytes.len() as u64,
                        data: Some(data),
                        download: None,
                    }],
                })
            } else {
                None
            };
            Ok::<(CaptureProposalCommit, HubCapture), String>((
                CaptureProposalCommit {
                    revision_id: outcome.revision.art_revision_id.to_string(),
                    status,
                    state_root: outcome.revision.state_root_hash.to_string(),
                    fanout,
                },
                hub_capture,
            ))
        })
        .await
        .map_err(|error| format!("Studio capture task failed: {error}"))??;
        if let Some(adapter) = self.state.hub_adapter.lock().await.clone() {
            adapter
                .publish_capture(hub_capture)
                .await
                .map_err(|error| {
                    format!("Hub proposal failed after local durable capture: {error}")
                })?;
        }
        Ok(commit)
    }
}

impl Default for DesktopState {
    fn default() -> Self {
        let public = restore_oauth_status();
        Self {
            oauth: Arc::new(Mutex::new(OAuthInternal { public })),
            oauth_cancel: Arc::new(Mutex::new(None)),
            roblox_resources: Arc::new(Mutex::new(RobloxResourcePublicStatus::default())),
            workspace: Arc::new(Mutex::new(None)),
            bridge_context_gate: Arc::new(Mutex::new(())),
            bridge_runtime: Arc::new(Mutex::new(None)),
            bridge_handle: Arc::new(Mutex::new(None)),
            bridge_ui: Arc::new(Mutex::new(BridgeUiInternal::default())),
            hub_adapter: Arc::new(Mutex::new(None)),
            hub_runtime: Arc::new(Mutex::new(None)),
            rojo_sessions: Arc::new(StdMutex::new(RojoSessionManager::default())),
            rojo_adapter: Arc::new(RojoDesktopConfigAdapter::workspace_scoped()),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct BridgeUiInternal {
    public: BridgePublicStatus,
    challenge_codes: HashMap<String, String>,
    sessions: HashSet<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgePublicStatus {
    available: bool,
    session_count: usize,
    discovery_address: Option<String>,
    session_address: Option<String>,
    pending_pairings: Vec<PairingPrompt>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingPrompt {
    challenge: String,
    pairing_code: String,
    plugin_installation_id: String,
    studio_user_id: Option<String>,
    studio_version: Option<String>,
    studio_platform: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct OAuthInternal {
    public: OAuthPublicStatus,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RobloxResourcePhase {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RobloxResourcePublicStatus {
    phase: RobloxResourcePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    inventory: Option<RobloxResourceInventory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection: Option<RobloxSelectionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct OAuthPublicStatus {
    phase: OAuthPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
enum OAuthPhase {
    #[default]
    SignedOut,
    Waiting,
    Exchanging,
    SignedIn,
    Failed,
}

enum VaultRestore {
    NoEntry,
    Secret(String),
    Failed,
}

fn restore_oauth_status() -> OAuthPublicStatus {
    let read = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT) {
        Ok(entry) => match entry.get_password() {
            Ok(secret) => VaultRestore::Secret(secret),
            Err(keyring::Error::NoEntry) => VaultRestore::NoEntry,
            Err(_) => VaultRestore::Failed,
        },
        Err(_) => VaultRestore::Failed,
    };
    oauth_status_from_vault(read)
}

fn oauth_status_from_vault(read: VaultRestore) -> OAuthPublicStatus {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    oauth_status_from_vault_at(read, now)
}

fn oauth_status_from_vault_at(read: VaultRestore, now: u64) -> OAuthPublicStatus {
    match read {
        VaultRestore::NoEntry => OAuthPublicStatus::default(),
        VaultRestore::Failed => OAuthPublicStatus {
            phase: OAuthPhase::Failed,
            account_name: None,
            account_id: None,
            message: Some(
                "OAuth credential vault access failed. NeuMan did not use a fallback store.".into(),
            ),
        },
        VaultRestore::Secret(secret) => match serde_json::from_str::<StoredOAuth>(&secret) {
            Ok(stored) if restored_oauth_record_is_valid(&stored, now) => {
                let access_expires_at = stored
                    .stored_at_unix_seconds
                    .saturating_add(stored.tokens.expires_in);
                OAuthPublicStatus {
                    phase: OAuthPhase::SignedIn,
                    account_name: stored.user.preferred_username.or(stored.user.name),
                    account_id: Some(stored.user.sub),
                    message: Some(if access_expires_at <= now {
                        "Protected Roblox session restored; rotate it before accessing resources."
                            .into()
                    } else {
                        "OAuth session restored from the operating-system credential vault.".into()
                    }),
                }
            }
            Err(_) => OAuthPublicStatus {
                phase: OAuthPhase::Failed,
                account_name: None,
                account_id: None,
                message: Some(
                    "OAuth credential vault data is invalid. Sign in again after clearing it."
                        .into(),
                ),
            },
            Ok(_) => OAuthPublicStatus {
                phase: OAuthPhase::Failed,
                account_name: None,
                account_id: None,
                message: Some(
                    "OAuth credential vault data failed schema, client, scope, or lifetime validation. Sign in again after clearing it."
                        .into(),
                ),
            },
        },
    }
}

fn restored_oauth_record_is_valid(stored: &StoredOAuth, now: u64) -> bool {
    stored.schema_version == 1
        && !stored.user.sub.is_empty()
        && stored.user.sub.len() <= 512
        && stored.stored_at_unix_seconds <= now.saturating_add(300)
        && RobloxPublicClientConfig::recommended(stored.client_id.clone()).is_ok()
        && validate_initial_token_set(&stored.tokens).is_ok()
        && COMPILED_OAUTH_CLIENT_ID
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none_or(|compiled| compiled == stored.client_id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    version: &'static str,
    cli_available: bool,
    bridge_connected: bool,
    bridge_session_count: usize,
    pending_pairing_count: usize,
    hub_connected: bool,
    workspace: Option<String>,
    rojo_session_count: usize,
    rojo_healthy_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    rojo_last_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartOAuthRequest {
    client_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OAuthClientConfiguration {
    client_id: Option<String>,
    build_provided: bool,
    redirect_uri: &'static str,
    pkce_method: &'static str,
    client_secret_embedded: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartOAuthResponse {
    redirect_uri: String,
    expires_in_seconds: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteActionRequest {
    action: String,
    path: Option<String>,
    place: Option<String>,
    art_revision: Option<String>,
    candidate: Option<String>,
    bundle_hash: Option<String>,
    environment: Option<String>,
    cell_slot: Option<String>,
    cell_path: Option<String>,
    message: Option<String>,
    remote: Option<String>,
    upstream: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationResult {
    ok: bool,
    operation_id: String,
    summary: String,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    jwks_uri: String,
}

#[derive(Deserialize, Serialize)]
struct OAuthTokenSet {
    access_token: String,
    refresh_token: String,
    id_token: String,
    token_type: String,
    expires_in: u64,
    scope: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UserInfo {
    sub: String,
    name: Option<String>,
    preferred_username: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct StoredOAuth {
    schema_version: u8,
    client_id: String,
    stored_at_unix_seconds: u64,
    tokens: OAuthTokenSet,
    user: UserInfo,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<EcJwk>,
}

#[derive(Debug, Deserialize)]
struct EcJwk {
    kid: String,
    kty: String,
    crv: String,
    x: String,
    y: String,
    alg: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct IdClaims {
    sub: String,
    iss: String,
    aud: serde_json::Value,
    exp: u64,
    iat: u64,
    nonce: String,
}

#[tauri::command]
async fn system_status(state: State<'_, DesktopState>) -> Result<SystemStatus, String> {
    let workspace = state
        .workspace
        .lock()
        .await
        .as_ref()
        .map(|path| path.display().to_string());
    let cli_available = cli_path().is_some_and(|path| path.exists());
    let bridge = state.bridge_ui.lock().await.public.clone();
    let manager = state.rojo_sessions.clone();
    let rojo = tokio::task::spawn_blocking(move || {
        let mut manager = manager
            .lock()
            .map_err(|_| "The Rojo session manager lock was poisoned.".to_owned())?;
        manager
            .list()
            .map_err(|error| format!("{}: {}", error.code, error.message))
    })
    .await
    .map_err(|error| format!("Rojo status task failed: {error}"));
    let (rojo_session_count, rojo_healthy_count, rojo_last_error) = match rojo {
        Ok(Ok(sessions)) => (
            sessions.len(),
            sessions
                .iter()
                .filter(|session| session.state == RojoServerState::Healthy)
                .count(),
            None,
        ),
        Ok(Err(error)) | Err(error) => (0, 0, Some(error)),
    };

    Ok(SystemStatus {
        version: env!("CARGO_PKG_VERSION"),
        cli_available,
        bridge_connected: bridge.available,
        bridge_session_count: bridge.session_count,
        pending_pairing_count: bridge.pending_pairings.len(),
        hub_connected: std::env::var_os("NEUMAN_HUB_URL").is_some(),
        workspace,
        rojo_session_count,
        rojo_healthy_count,
        rojo_last_error,
    })
}

#[tauri::command]
async fn oauth_status(state: State<'_, DesktopState>) -> Result<OAuthPublicStatus, String> {
    Ok(state.oauth.lock().await.public.clone())
}

struct ProtectedRobloxResourceSession {
    access_token: OAuthSecret,
    client_id: String,
}

fn read_protected_oauth_record() -> Result<StoredOAuth, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|_| "Could not access the operating-system credential vault.".to_owned())?;
    let serialized = entry.get_password().map_err(|error| match error {
        keyring::Error::NoEntry => {
            "No protected Roblox session exists; sign in interactively.".to_owned()
        }
        _ => "Could not read the protected Roblox session; no fallback was used.".to_owned(),
    })?;
    let stored: StoredOAuth = serde_json::from_str(&serialized)
        .map_err(|_| "The protected Roblox session record is invalid.".to_owned())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "The local clock is before the Unix epoch.".to_owned())?
        .as_secs();
    if !restored_oauth_record_is_valid(&stored, now) {
        return Err(
            "The protected Roblox session failed schema, client, or scope validation.".into(),
        );
    }
    Ok(stored)
}

async fn protected_roblox_resource_session(
    state: &DesktopState,
) -> Result<ProtectedRobloxResourceSession, String> {
    let mut stored = read_protected_oauth_record()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "The local clock is before the Unix epoch.".to_owned())?
        .as_secs();
    let refresh_before = stored
        .stored_at_unix_seconds
        .saturating_add(stored.tokens.expires_in)
        .saturating_sub(60);
    if now >= refresh_before {
        refresh_oauth_state(state.oauth.clone()).await?;
        stored = read_protected_oauth_record()?;
    }
    let access_token = OAuthSecret::new(stored.tokens.access_token)
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    Ok(ProtectedRobloxResourceSession {
        access_token,
        client_id: stored.client_id,
    })
}

#[tauri::command]
async fn roblox_resource_status(
    state: State<'_, DesktopState>,
) -> Result<RobloxResourcePublicStatus, String> {
    Ok(state.roblox_resources.lock().await.clone())
}

async fn set_resource_failure(state: &DesktopState, error: String) {
    *state.roblox_resources.lock().await = RobloxResourcePublicStatus {
        phase: RobloxResourcePhase::Failed,
        inventory: None,
        selection: None,
        last_error: Some(error),
    };
}

#[tauri::command]
async fn refresh_roblox_resources(
    state: State<'_, DesktopState>,
) -> Result<RobloxResourcePublicStatus, String> {
    *state.roblox_resources.lock().await = RobloxResourcePublicStatus {
        phase: RobloxResourcePhase::Loading,
        ..RobloxResourcePublicStatus::default()
    };
    let result: Result<RobloxResourceInventory, String> = async {
        let session = protected_roblox_resource_session(&state).await?;
        let transport = ReqwestRobloxResourceTransport::new()
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        RobloxResourceProvider::new(transport)
            .discover(&session.access_token, &session.client_id)
            .await
            .map_err(|error| format!("{}: {}", error.code, error.message))
    }
    .await;
    match result {
        Ok(inventory) => {
            let public = RobloxResourcePublicStatus {
                phase: RobloxResourcePhase::Ready,
                inventory: Some(inventory),
                selection: None,
                last_error: None,
            };
            *state.roblox_resources.lock().await = public.clone();
            Ok(public)
        }
        Err(error) => {
            set_resource_failure(&state, error.clone()).await;
            Err(error)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeRobloxUniverseRequest {
    universe_id: String,
}

#[tauri::command]
async fn probe_roblox_universe(
    state: State<'_, DesktopState>,
    request: ProbeRobloxUniverseRequest,
) -> Result<RobloxResourcePublicStatus, String> {
    let result: Result<RobloxResourceInventory, String> = async {
        let session = protected_roblox_resource_session(&state).await?;
        let transport = ReqwestRobloxResourceTransport::new()
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        let provider = RobloxResourceProvider::new(transport);
        let mut inventory = provider
            .discover(&session.access_token, &session.client_id)
            .await
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        if !inventory
            .universes
            .iter()
            .any(|universe| universe.id == request.universe_id.trim())
        {
            let universe = provider
                .probe_universe(
                    &session.access_token,
                    &session.client_id,
                    request.universe_id.trim(),
                )
                .await
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
            inventory.universes.push(universe);
            inventory.universes.sort_by(|left, right| {
                left.id
                    .len()
                    .cmp(&right.id.len())
                    .then(left.id.cmp(&right.id))
            });
        }
        Ok(inventory)
    }
    .await;
    match result {
        Ok(inventory) => {
            let public = RobloxResourcePublicStatus {
                phase: RobloxResourcePhase::Ready,
                inventory: Some(inventory),
                selection: None,
                last_error: None,
            };
            *state.roblox_resources.lock().await = public.clone();
            Ok(public)
        }
        Err(error) => {
            set_resource_failure(&state, error.clone()).await;
            Err(error)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectRobloxPlaceRequest {
    universe_id: String,
    place_id: String,
}

#[tauri::command]
async fn select_roblox_place(
    state: State<'_, DesktopState>,
    request: SelectRobloxPlaceRequest,
) -> Result<RobloxResourcePublicStatus, String> {
    let result: Result<RobloxSelectionEvidence, String> = async {
        let session = protected_roblox_resource_session(&state).await?;
        let transport = ReqwestRobloxResourceTransport::new()
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        let observed_at_unix_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "The local clock is before the Unix epoch.".to_owned())?
            .as_secs();
        RobloxResourceProvider::new(transport)
            .read_selection(
                &session.access_token,
                &session.client_id,
                request.universe_id.trim(),
                request.place_id.trim(),
                observed_at_unix_seconds,
            )
            .await
            .map_err(|error| format!("{}: {}", error.code, error.message))
    }
    .await;
    match result {
        Ok(selection) => {
            let mut public = state.roblox_resources.lock().await;
            if let Some(inventory) = public.inventory.as_mut()
                && let Some(universe) = inventory
                    .universes
                    .iter_mut()
                    .find(|universe| universe.id == selection.universe.id)
            {
                if let Some(place) = universe
                    .places
                    .iter_mut()
                    .find(|place| place.id == selection.place.id)
                {
                    *place = selection.place.clone();
                } else {
                    universe.places.push(selection.place.clone());
                }
            }
            public.phase = RobloxResourcePhase::Ready;
            public.selection = Some(selection);
            public.last_error = None;
            Ok(public.clone())
        }
        Err(error) => {
            set_resource_failure(&state, error.clone()).await;
            Err(error)
        }
    }
}

#[tauri::command]
async fn clear_roblox_resource_selection(
    state: State<'_, DesktopState>,
) -> Result<RobloxResourcePublicStatus, String> {
    let mut public = state.roblox_resources.lock().await;
    public.selection = None;
    Ok(public.clone())
}

async fn refresh_oauth_state(
    oauth_state: Arc<Mutex<OAuthInternal>>,
) -> Result<OAuthPublicStatus, String> {
    {
        let mut oauth = oauth_state.lock().await;
        if matches!(
            oauth.public.phase,
            OAuthPhase::Waiting | OAuthPhase::Exchanging
        ) {
            return Err("An OAuth authorization or refresh is already active.".into());
        }
        oauth.public = OAuthPublicStatus {
            phase: OAuthPhase::Exchanging,
            message: Some("Rotating and validating the protected Roblox session.".into()),
            ..OAuthPublicStatus::default()
        };
    }

    let result: Result<OAuthPublicStatus, String> = async {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .map_err(|_| "Could not access the operating-system credential vault.".to_owned())?;
        let serialized = entry.get_password().map_err(|error| match error {
            keyring::Error::NoEntry => {
                "No protected Roblox session exists; sign in interactively.".to_owned()
            }
            _ => "Could not read the protected Roblox session; no fallback was used.".to_owned(),
        })?;
        let stored: StoredOAuth = serde_json::from_str(&serialized)
            .map_err(|_| "The protected Roblox session record is invalid.".to_owned())?;
        if stored.schema_version != 1 {
            return Err("The protected Roblox session schema is unsupported.".into());
        }
        if let Some(compiled) = COMPILED_OAUTH_CLIENT_ID
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && compiled != stored.client_id
        {
            return Err(
                "The protected session belongs to a different OAuth public client; sign in again."
                    .into(),
            );
        }

        let config = RobloxPublicClientConfig::recommended(stored.client_id.clone())
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        let context = RobloxRefreshContext::new(
            OAuthSecret::new(stored.tokens.refresh_token.clone())
                .map_err(|error| format!("{}: {}", error.code, error.message))?,
            stored.user.sub.clone(),
        )
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
        let transport = ReqwestRobloxOAuthTransport::new()
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        let refresher = RobloxOAuthRefresher::new(config, transport);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "The local clock is before the Unix epoch.".to_owned())?
            .as_secs();
        let rotated = refresher
            .refresh(context, now)
            .await
            .map_err(|error| format!("{}: {}", error.code, error.message))?;

        let user = UserInfo {
            sub: rotated.user().sub.clone(),
            name: rotated.user().name.clone(),
            preferred_username: rotated.user().preferred_username.clone(),
        };
        let tokens = OAuthTokenSet {
            access_token: rotated.access_token().expose_secret().to_owned(),
            refresh_token: rotated.refresh_token().expose_secret().to_owned(),
            id_token: rotated
                .id_token()
                .map(|token| token.expose_secret().to_owned())
                .unwrap_or(stored.tokens.id_token),
            token_type: "Bearer".into(),
            expires_in: rotated.expires_in_seconds(),
            scope: rotated.scopes().iter().cloned().collect::<Vec<_>>().join(" "),
        };
        let refreshed = StoredOAuth {
            schema_version: 1,
            client_id: stored.client_id,
            stored_at_unix_seconds: now,
            tokens,
            user: user.clone(),
        };
        let serialized = serde_json::to_string(&refreshed)
            .map_err(|_| "Could not encode the protected Roblox session.".to_owned())?;
        entry.set_password(&serialized).map_err(|_| {
            "The rotated Roblox session could not be committed to the OS credential vault; sign in again."
                .to_owned()
        })?;
        Ok(OAuthPublicStatus {
            phase: OAuthPhase::SignedIn,
            account_name: user.preferred_username.or(user.name),
            account_id: Some(user.sub),
            message: Some("Roblox session rotated in the OS credential vault.".into()),
        })
    }
    .await;

    match result {
        Ok(public) => {
            oauth_state.lock().await.public = public.clone();
            Ok(public)
        }
        Err(error) => {
            oauth_state.lock().await.public = OAuthPublicStatus {
                phase: OAuthPhase::Failed,
                message: Some(error.clone()),
                ..OAuthPublicStatus::default()
            };
            Err(error)
        }
    }
}

#[tauri::command]
async fn refresh_roblox_oauth(state: State<'_, DesktopState>) -> Result<OAuthPublicStatus, String> {
    refresh_oauth_state(state.oauth.clone()).await
}

#[tauri::command]
fn oauth_client_configuration() -> OAuthClientConfiguration {
    let client_id = COMPILED_OAUTH_CLIENT_ID
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    OAuthClientConfiguration {
        build_provided: client_id.is_some(),
        client_id,
        redirect_uri: OAUTH_REDIRECT_URI,
        pkce_method: "S256",
        client_secret_embedded: false,
    }
}

#[tauri::command]
async fn bridge_status(state: State<'_, DesktopState>) -> Result<BridgePublicStatus, String> {
    Ok(state.bridge_ui.lock().await.public.clone())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovePairingRequest {
    challenge: String,
    plugin_installation_id: String,
}

#[tauri::command]
async fn approve_studio_pairing(
    state: State<'_, DesktopState>,
    request: ApprovePairingRequest,
) -> Result<(), String> {
    let known = state
        .bridge_ui
        .lock()
        .await
        .public
        .pending_pairings
        .iter()
        .any(|prompt| {
            prompt.challenge == request.challenge
                && prompt.plugin_installation_id == request.plugin_installation_id
        });
    if !known {
        return Err("The Studio pairing request is no longer pending.".into());
    }
    let handle = state
        .bridge_handle
        .lock()
        .await
        .clone()
        .ok_or_else(|| "The Studio bridge is not available.".to_owned())?;
    handle
        .approve_pairing(&request.challenge, &request.plugin_installation_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn start_roblox_oauth(
    state: State<'_, DesktopState>,
    request: StartOAuthRequest,
) -> Result<StartOAuthResponse, String> {
    let client_id = COMPILED_OAUTH_CLIENT_ID
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| request.client_id.as_deref().map(str::trim))
        .unwrap_or_default()
        .to_owned();
    if client_id.is_empty()
        || client_id.len() > 128
        || !client_id.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return Err("The OAuth client ID is invalid.".into());
    }

    {
        let oauth = state.oauth.lock().await;
        if matches!(
            oauth.public.phase,
            OAuthPhase::Waiting | OAuthPhase::Exchanging
        ) {
            return Err("An OAuth authorization is already active.".into());
        }
    }

    if let Some(stale) = state.oauth_cancel.lock().await.take() {
        let _ = stale.send(true);
    }

    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, OAUTH_CALLBACK_PORT))
        .await
        .map_err(|error| format!("Could not bind the loopback OAuth callback: {error}"))?;
    let redirect_uri = OAUTH_REDIRECT_URI.to_owned();
    let verifier = random_url_token(64);
    let state_token = random_url_token(32);
    let nonce = random_url_token(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let mut authorize = Url::parse("https://apis.roblox.com/oauth/v1/authorize")
        .map_err(|error| error.to_string())?;
    authorize
        .query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", "openid profile")
        .append_pair("response_type", "code")
        .append_pair("state", &state_token)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "select_account");

    {
        let mut oauth = state.oauth.lock().await;
        oauth.public = OAuthPublicStatus {
            phase: OAuthPhase::Waiting,
            message: Some("Waiting for the browser authorization callback.".into()),
            ..OAuthPublicStatus::default()
        };
    }
    *state.roblox_resources.lock().await = RobloxResourcePublicStatus::default();

    if let Err(error) = open_system_browser(authorize.as_str()) {
        state.oauth.lock().await.public = OAuthPublicStatus {
            phase: OAuthPhase::Failed,
            message: Some(error.clone()),
            ..OAuthPublicStatus::default()
        };
        return Err(error);
    }
    let (cancel_sender, mut cancel_receiver) = watch::channel(false);
    *state.oauth_cancel.lock().await = Some(cancel_sender.clone());
    let oauth_state = state.oauth.clone();
    let cancel_slot = state.oauth_cancel.clone();
    let redirect_for_task = redirect_uri.clone();
    tauri::async_runtime::spawn(async move {
        let authorization = tokio::time::timeout(
            Duration::from_secs(300),
            finish_oauth(
                listener,
                client_id,
                redirect_for_task,
                verifier,
                state_token,
                nonce,
                oauth_state.clone(),
            ),
        );
        tokio::pin!(authorization);
        let result = tokio::select! {
            result = &mut authorization => Some(result),
            _ = cancel_receiver.changed() => None,
        };
        if let Some(result) = result
            && let Err(message) =
                result.unwrap_or_else(|_| Err("OAuth authorization timed out.".into()))
        {
            let mut oauth = oauth_state.lock().await;
            oauth.public = OAuthPublicStatus {
                phase: OAuthPhase::Failed,
                message: Some(message),
                ..OAuthPublicStatus::default()
            };
        }
        let mut active = cancel_slot.lock().await;
        if active
            .as_ref()
            .is_some_and(|sender| sender.same_channel(&cancel_sender))
        {
            active.take();
        }
    });

    Ok(StartOAuthResponse {
        redirect_uri,
        expires_in_seconds: 300,
    })
}

#[tauri::command]
async fn cancel_roblox_oauth(state: State<'_, DesktopState>) -> Result<(), String> {
    if let Some(sender) = state.oauth_cancel.lock().await.take() {
        let _ = sender.send(true);
    }
    let mut oauth = state.oauth.lock().await;
    if matches!(
        oauth.public.phase,
        OAuthPhase::Waiting | OAuthPhase::Exchanging
    ) {
        oauth.public = OAuthPublicStatus::default();
    }
    drop(oauth);
    *state.roblox_resources.lock().await = RobloxResourcePublicStatus::default();
    Ok(())
}

async fn finish_oauth(
    listener: TcpListener,
    client_id: String,
    redirect_uri: String,
    verifier: String,
    expected_state: String,
    expected_nonce: String,
    oauth_state: Arc<Mutex<OAuthInternal>>,
) -> Result<(), String> {
    let (mut stream, peer) = listener.accept().await.map_err(|error| error.to_string())?;
    if !peer.ip().is_loopback() {
        return Err("Rejected a non-loopback OAuth callback.".into());
    }
    let mut request = vec![0_u8; 8_192];
    let read = stream
        .read(&mut request)
        .await
        .map_err(|error| error.to_string())?;
    if read == request.len() {
        return Err("The OAuth callback exceeded the size limit.".into());
    }
    let request = std::str::from_utf8(&request[..read])
        .map_err(|_| "The OAuth callback was not UTF-8.".to_owned())?;
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| "The OAuth callback was empty.".to_owned())?;
    let target = first_line
        .strip_prefix("GET ")
        .and_then(|rest| rest.split_once(' ').map(|(target, _)| target))
        .ok_or_else(|| "The OAuth callback request was malformed.".to_owned())?;
    let host = request
        .lines()
        .skip(1)
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        })
        .map(|(_, value)| value.trim());
    if host != Some("localhost:43891") {
        return Err(
            "The OAuth callback Host header did not match the registered loopback origin.".into(),
        );
    }
    let callback = Url::parse(&format!("http://localhost:{OAUTH_CALLBACK_PORT}{target}"))
        .map_err(|_| "The OAuth callback URL was malformed.".to_owned())?;
    if callback.path() != "/oauth/callback" {
        return Err("The OAuth callback path did not match.".into());
    }
    let mut values = std::collections::HashMap::<String, String>::new();
    for (key, value) in callback.query_pairs() {
        if values
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err("The OAuth callback contained a duplicate parameter.".into());
        }
    }
    if values.get("state") != Some(&expected_state) {
        return Err("The OAuth state did not match; authorization was rejected.".into());
    }
    if let Some(provider_error) = values.get("error") {
        return Err(format!("Roblox denied authorization: {provider_error}"));
    }
    let code = values
        .get("code")
        .cloned()
        .ok_or_else(|| "The OAuth callback contained no code.".to_owned())?;
    if values.len() > 2 {
        return Err("The OAuth callback contained unexpected parameters.".into());
    }

    let body = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Connected to NeuMan</title>
<style>
  :root { color-scheme: dark; font-family: Inter, system-ui, sans-serif; background: #090b10; color: #f7f8fb; }
  body { min-height: 100vh; margin: 0; display: grid; place-items: center; background: radial-gradient(circle at 50% 10%, #17234b, #090b10 48%); }
  main { width: min(420px, calc(100% - 40px)); text-align: center; }
  .check { width: 68px; height: 68px; margin: 0 auto 24px; display: grid; place-items: center; border-radius: 20px; background: #59cf8c; color: #07130d; font-size: 34px; font-weight: 800; }
  h1 { margin: 0; font-size: 32px; letter-spacing: -0.04em; }
  p { margin: 14px 0 0; color: #9da5b5; line-height: 1.6; }
</style>
<body><main><div class="check">✓</div><h1>Connected to NeuMan</h1><p>Authorization was received. You can close this tab and return to the app.</p></main></body>
</html>"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stream.shutdown().await.map_err(|error| error.to_string())?;

    oauth_state.lock().await.public = OAuthPublicStatus {
        phase: OAuthPhase::Exchanging,
        message: Some("Validating the Roblox identity response.".into()),
        ..OAuthPublicStatus::default()
    };

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not construct the bounded Roblox OAuth client.".to_owned())?;
    let discovery_response = http
        .get(OAUTH_DISCOVERY)
        .send()
        .await
        .map_err(|_| "Roblox OAuth discovery was unavailable.".to_owned())?;
    let discovery: DiscoveryDocument =
        bounded_oauth_json(discovery_response, "Roblox OAuth discovery").await?;
    validate_discovery(&discovery)?;

    let token_response = http
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "Roblox OAuth token exchange was unavailable.".to_owned())?;
    let tokens: OAuthTokenSet =
        bounded_oauth_json(token_response, "Roblox OAuth token exchange").await?;
    validate_initial_token_set(&tokens)?;

    let jwks_response = http
        .get(&discovery.jwks_uri)
        .send()
        .await
        .map_err(|_| "Roblox OAuth signing keys were unavailable.".to_owned())?;
    let jwks: JwkSet = bounded_oauth_json(jwks_response, "Roblox OAuth signing keys").await?;
    let claims = validate_id_token(&tokens.id_token, &client_id, &expected_nonce, &jwks)?;

    let user_response = http
        .get(&discovery.userinfo_endpoint)
        .bearer_auth(&tokens.access_token)
        .send()
        .await
        .map_err(|_| "Roblox OAuth user information was unavailable.".to_owned())?;
    let user: UserInfo = bounded_oauth_json(user_response, "Roblox OAuth user information").await?;
    if user.sub != claims.sub {
        return Err("Roblox user information did not match the signed identity token.".into());
    }

    let stored = StoredOAuth {
        schema_version: 1,
        client_id,
        stored_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "The local clock is before the Unix epoch.".to_owned())?
            .as_secs(),
        tokens,
        user: user.clone(),
    };
    let serialized = serde_json::to_string(&stored).map_err(|error| error.to_string())?;
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|error| {
            format!("Could not access the operating-system credential vault: {error}")
        })?
        .set_password(&serialized)
        .map_err(|error| {
            format!("Could not store the OAuth session in the credential vault: {error}")
        })?;

    oauth_state.lock().await.public = OAuthPublicStatus {
        phase: OAuthPhase::SignedIn,
        account_name: user.preferred_username.or(user.name),
        account_id: Some(user.sub),
        message: None,
    };
    Ok(())
}

#[tauri::command]
async fn logout_roblox(state: State<'_, DesktopState>) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| {
        format!("Could not access the operating-system credential vault: {error}")
    })?;
    let mut remote_revocation_unconfirmed = false;
    if let Ok(value) = entry.get_password()
        && let Ok(stored) = serde_json::from_str::<StoredOAuth>(&value)
    {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not construct the bounded Roblox OAuth client.".to_owned())?;
        let response = client
            .post(OAUTH_REVOCATION_ENDPOINT)
            .form(&[
                ("token", stored.tokens.refresh_token.as_str()),
                ("client_id", stored.client_id.as_str()),
            ])
            .send()
            .await;
        let revocation_confirmed = match response {
            Ok(response) => bounded_oauth_body(response, "Roblox OAuth revocation")
                .await
                .is_ok(),
            Err(_) => false,
        };
        remote_revocation_unconfirmed = !revocation_confirmed;
    }
    entry
        .delete_credential()
        .map_err(|error| format!("Could not delete the OAuth credential: {error}"))?;
    state.oauth.lock().await.public = OAuthPublicStatus::default();
    *state.roblox_resources.lock().await = RobloxResourcePublicStatus::default();
    if remote_revocation_unconfirmed {
        Err("Signed out locally, but Roblox refresh-token revocation could not be confirmed; review authorized applications in Roblox account settings.".into())
    } else {
        Ok(())
    }
}

fn validate_discovery(discovery: &DiscoveryDocument) -> Result<(), String> {
    if discovery.issuer != OAUTH_ISSUER {
        return Err("Roblox OAuth discovery returned an unexpected issuer.".into());
    }
    if discovery.token_endpoint != OAUTH_TOKEN_ENDPOINT
        || discovery.userinfo_endpoint != OAUTH_USERINFO_ENDPOINT
        || discovery.jwks_uri != OAUTH_JWKS_ENDPOINT
    {
        return Err(
            "OAuth discovery returned an endpoint outside the pinned Roblox contract.".into(),
        );
    }
    Ok(())
}

fn validate_initial_token_set(tokens: &OAuthTokenSet) -> Result<(), String> {
    for secret in [
        tokens.access_token.as_str(),
        tokens.refresh_token.as_str(),
        tokens.id_token.as_str(),
    ] {
        OAuthSecret::new(secret.to_owned())
            .map_err(|_| "Roblox returned an empty, oversized, or malformed token.".to_owned())?;
    }
    if !tokens.token_type.eq_ignore_ascii_case("bearer") || tokens.expires_in < 60 {
        return Err("Roblox returned an invalid token type or lifetime.".into());
    }
    let scopes = tokens
        .scope
        .split_ascii_whitespace()
        .collect::<HashSet<_>>();
    if REQUIRED_OAUTH_SCOPES
        .iter()
        .any(|required| !scopes.contains(required))
    {
        return Err("Roblox returned an OAuth scope downgrade.".into());
    }
    Ok(())
}

async fn bounded_oauth_json<T: DeserializeOwned>(
    response: reqwest::Response,
    operation: &'static str,
) -> Result<T, String> {
    let body = bounded_oauth_body(response, operation).await?;
    serde_json::from_slice(&body).map_err(|_| format!("{operation} returned invalid JSON."))
}

async fn bounded_oauth_body(
    response: reqwest::Response,
    operation: &'static str,
) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(format!("{operation} failed."));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_OAUTH_RESPONSE_BYTES as u64)
    {
        return Err(format!("{operation} exceeded the response limit."));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| format!("{operation} response was interrupted."))?;
        if body.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
            return Err(format!("{operation} exceeded the response limit."));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_id_token(
    token: &str,
    audience: &str,
    nonce: &str,
    jwks: &JwkSet,
) -> Result<IdClaims, String> {
    let header =
        decode_header(token).map_err(|_| "The Roblox ID token header was invalid.".to_owned())?;
    if header.alg != Algorithm::ES256 {
        return Err("The Roblox ID token used an unexpected signing algorithm.".into());
    }
    let kid = header
        .kid
        .ok_or_else(|| "The Roblox ID token had no key ID.".to_owned())?;
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .ok_or_else(|| "The Roblox ID token signing key was not found.".to_owned())?;
    if key.kty != "EC" || key.crv != "P-256" || key.alg.as_deref().is_some_and(|alg| alg != "ES256")
    {
        return Err("The Roblox signing key type was not accepted.".into());
    }
    let decoding_key = DecodingKey::from_ec_components(&key.x, &key.y)
        .map_err(|_| "The Roblox signing key was invalid.".to_owned())?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_issuer(&[OAUTH_ISSUER]);
    validation.set_audience(&[audience]);
    validation.leeway = 60;
    let claims = decode::<IdClaims>(token, &decoding_key, &validation)
        .map_err(|_| "The Roblox ID token signature or claims were invalid.".to_owned())?
        .claims;
    if claims.nonce != nonce || claims.iss != OAUTH_ISSUER || claims.exp <= claims.iat {
        return Err("The Roblox ID token correlation claims were invalid.".into());
    }
    let _audience_shape_checked_by_library = &claims.aud;
    Ok(claims)
}

fn selected_place_key(workspace: &Path, requested: Option<&str>) -> Result<String, String> {
    let (manifest, _) = ProjectManifest::load(workspace).map_err(|report| {
        report
            .errors
            .into_iter()
            .take(8)
            .map(|issue| format!("{} {}: {}", issue.code, issue.path, issue.message))
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let place_key =
        match requested.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => safe_key(value, "place")?,
            None => manifest.project.default_place.clone().ok_or_else(|| {
                "Choose a place because project.defaultPlace is not set.".to_owned()
            })?,
        };
    if !manifest.places.contains_key(&place_key) {
        return Err(format!(
            "The selected place `{place_key}` is not present in the validated manifest."
        ));
    }
    Ok(place_key)
}

async fn execute_rojo_action(
    state: &DesktopState,
    workspace: PathBuf,
    action: String,
    requested_place: Option<String>,
) -> Result<OperationResult, String> {
    let sessions = state.rojo_sessions.clone();
    let adapter = state.rojo_adapter.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let (summary, stdout) = tokio::task::spawn_blocking(move || {
        let mut sessions = sessions
            .lock()
            .map_err(|_| "The Rojo session manager lock was poisoned.".to_owned())?;
        match action.as_str() {
            "rojo-start" => {
                let resolved = adapter
                    .resolve(&RojoDesktopSelection {
                        workspace_root: workspace,
                        place_key: requested_place,
                    })
                    .map_err(|error| format!("{}: {}", error.code, error.message))?;
                let outcome = sessions
                    .start(resolved.session_request())
                    .map_err(|error| format!("{}: {}", error.code, error.message))?;
                let summary = if outcome.created {
                    format!(
                        "Started pinned Rojo live sync for `{}` on 127.0.0.1:{}.",
                        outcome.status.key.place_key(),
                        outcome.status.port
                    )
                } else {
                    format!(
                        "Reused the existing pinned Rojo session for `{}` on 127.0.0.1:{}.",
                        outcome.status.key.place_key(),
                        outcome.status.port
                    )
                };
                let stdout = serde_json::to_string(&outcome).map_err(|error| error.to_string())?;
                Ok((summary, stdout))
            }
            "rojo-list" => {
                let statuses = sessions
                    .list()
                    .map_err(|error| format!("{}: {}", error.code, error.message))?;
                let healthy = statuses
                    .iter()
                    .filter(|status| status.state == RojoServerState::Healthy)
                    .count();
                let summary = format!(
                    "Observed {} retained Rojo session(s); {healthy} healthy.",
                    statuses.len()
                );
                let stdout = serde_json::to_string(&statuses).map_err(|error| error.to_string())?;
                Ok((summary, stdout))
            }
            "rojo-status" | "rojo-stop" | "rojo-restart" => {
                let place_key = if action == "rojo-restart" {
                    adapter
                        .resolve(&RojoDesktopSelection {
                            workspace_root: workspace.clone(),
                            place_key: requested_place.clone(),
                        })
                        .map_err(|error| format!("{}: {}", error.code, error.message))?
                        .place_key()
                        .to_owned()
                } else {
                    selected_place_key(&workspace, requested_place.as_deref())?
                };
                let key = RojoSessionKey::create(&workspace, &place_key)
                    .map_err(|error| format!("{}: {}", error.code, error.message))?;
                let status = match action.as_str() {
                    "rojo-stop" => sessions.stop(&key),
                    "rojo-restart" => sessions.restart(&key),
                    _ => sessions.status_by_key(&key),
                }
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
                let verb = match action.as_str() {
                    "rojo-stop" => "Stopped",
                    "rojo-restart" => "Restarted",
                    _ => "Observed",
                };
                let summary = format!(
                    "{verb} Rojo session for `{}`: {:?} on 127.0.0.1:{}.",
                    status.key.place_key(),
                    status.state,
                    status.port
                );
                let stdout = serde_json::to_string(&status).map_err(|error| error.to_string())?;
                Ok((summary, stdout))
            }
            _ => Err("The Rojo operation is not allowlisted.".to_owned()),
        }
    })
    .await
    .map_err(|error| format!("Rojo operation task failed: {error}"))??;

    Ok(OperationResult {
        ok: true,
        operation_id,
        summary,
        stdout,
        stderr: String::new(),
    })
}

#[tauri::command]
async fn execute_action(
    state: State<'_, DesktopState>,
    request: ExecuteActionRequest,
) -> Result<OperationResult, String> {
    if request.action == "select-workspace" {
        let requested = request
            .path
            .ok_or_else(|| "A workspace path is required.".to_owned())?;
        let canonical = std::fs::canonicalize(&requested)
            .map_err(|error| format!("Could not open the workspace: {error}"))?;
        if !canonical.join("neuman.project.yaml").is_file() {
            return Err("The selected directory does not contain neuman.project.yaml.".into());
        }
        let binding = workspace_binding(&canonical)?;
        let bridge_handle = state.bridge_handle.lock().await.clone();
        let next_hub_adapter = match (&binding, &bridge_handle) {
            (Some(binding), Some(handle)) => {
                HubDesktopConfig::from_workspace_environment(&canonical, binding)
                    .map_err(|error| error.to_string())?
                    .map(|config| HubDesktopAdapter::open(config, handle.clone()))
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .map(Arc::new)
            }
            (Some(_), None) => {
                return Err("The Studio bridge is not ready; retry workspace selection.".into());
            }
            (None, _) => None,
        };
        let _context_guard = state.bridge_context_gate.lock().await;
        if let Some(runtime) = state.hub_runtime.lock().await.take() {
            let _ = runtime.shutdown.send(true);
            runtime.task.abort();
        }
        *state.hub_adapter.lock().await = None;
        if let Some(handle) = &bridge_handle {
            handle.set_binding(None).await;
        }
        *state.workspace.lock().await = Some(canonical.clone());
        if let Some(handle) = &bridge_handle {
            handle.set_binding(binding.clone()).await;
        }
        if let Some(adapter) = next_hub_adapter {
            let (shutdown, receiver) = watch::channel(false);
            let running = adapter.clone();
            let task = tokio::spawn(async move { running.run(receiver).await });
            *state.hub_adapter.lock().await = Some(adapter);
            *state.hub_runtime.lock().await = Some(DesktopHubRuntime { shutdown, task });
        }
        return Ok(OperationResult {
            ok: true,
            operation_id: uuid::Uuid::now_v7().to_string(),
            summary: if binding.is_some() {
                format!(
                    "Connected workspace {} and bound the Studio bridge.",
                    canonical.display()
                )
            } else {
                format!(
                    "Connected workspace {}; configure default place and art channel to enable Studio mutation traffic.",
                    canonical.display()
                )
            },
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    if request.action == "bridge-status" {
        let status = state.bridge_ui.lock().await.public.clone();
        let stdout = serde_json::to_string(&status).map_err(|error| error.to_string())?;
        return Ok(OperationResult {
            ok: status.available,
            operation_id: uuid::Uuid::now_v7().to_string(),
            summary: if status.available {
                format!(
                    "Studio bridge is ready with {} authenticated session(s).",
                    status.session_count
                )
            } else {
                status
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "The Studio bridge is unavailable.".into())
            },
            stdout,
            stderr: String::new(),
        });
    }

    let workspace = state
        .workspace
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Connect a project workspace before running this action.".to_owned())?;
    if matches!(
        request.action.as_str(),
        "rojo-start" | "rojo-list" | "rojo-status" | "rojo-stop" | "rojo-restart"
    ) {
        return execute_rojo_action(state.inner(), workspace, request.action, request.place).await;
    }
    let mut args = vec!["--json".to_owned()];
    match request.action.as_str() {
        "validate" => args.extend(strings(["project", "validate"])),
        "status" => args.push("status".into()),
        "code-sync" => {
            args.extend(strings(["code", "sync"]));
            if let Some(remote) = request
                .remote
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                args.extend(["--remote".into(), safe_git_ref(remote, "remote")?]);
            }
            if let Some(upstream) = request
                .upstream
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                args.extend(["--upstream".into(), safe_git_ref(upstream, "upstream")?]);
            }
        }
        "build" => {
            args.extend(strings(["build", "create", "--art"]));
            args.push(required_id(
                request.art_revision.as_deref(),
                "art revision",
                "art_",
            )?);
            if let Some(place) = request
                .place
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                args.extend(["--place".into(), safe_key(place, "place")?]);
            }
            if let Some(candidate) = request
                .candidate
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                let candidate = project_file(&workspace, candidate)?;
                args.extend(["--candidate".into(), candidate.display().to_string()]);
            }
        }
        "art-capture" => {
            let slot = request
                .cell_slot
                .as_deref()
                .ok_or_else(|| "A DataModel cell slot is required.".to_owned())?
                .trim();
            if !slot.starts_with('/') || slot.len() > 512 || slot.chars().any(char::is_control) {
                return Err("The cell slot must be an absolute, printable DataModel path.".into());
            }
            let cell_path = request
                .cell_path
                .as_deref()
                .ok_or_else(|| "A project-relative RBXM file is required.".to_owned())?;
            let cell_path = project_file(&workspace, cell_path)?;
            let message = safe_text(request.message.as_deref(), "checkpoint message", 500)?;
            args.extend(strings(["art", "capture", "--cell"]));
            args.push(format!("{slot}={}", cell_path.display()));
            args.extend(["--message".into(), message]);
        }
        "release-plan" => {
            args.extend(strings(["release", "plan", "--bundle"]));
            args.push(required_id(
                request.bundle_hash.as_deref(),
                "bundle hash",
                "b3-256:",
            )?);
            args.push("--environment".into());
            args.push(safe_key(
                request
                    .environment
                    .as_deref()
                    .ok_or_else(|| "An environment is required.".to_owned())?,
                "environment",
            )?);
            if let Some(place) = request
                .place
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                args.extend(["--place".into(), safe_key(place, "place")?]);
            }
        }
        _ => return Err("The requested desktop action is not allowlisted.".into()),
    }
    let cli =
        cli_path().ok_or_else(|| "The NeuMan CLI executable could not be located.".to_owned())?;
    let output = Command::new(cli)
        .args(&args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("Could not start the NeuMan core: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let summary = operation_summary(
        request.action.as_str(),
        output.status.success(),
        &stdout,
        &stderr,
    );
    Ok(OperationResult {
        ok: output.status.success(),
        operation_id: uuid::Uuid::now_v7().to_string(),
        summary,
        stdout,
        stderr,
    })
}

fn strings<const N: usize>(values: [&str; N]) -> impl Iterator<Item = String> {
    values.into_iter().map(str::to_owned)
}

fn safe_key(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(format!("The {label} key is invalid."));
    }
    Ok(value.to_owned())
}

fn safe_git_ref(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 200
        || value.starts_with('-')
        || value.contains("..")
        || value.contains("@{")
        || value.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
    {
        return Err(format!("The Git {label} is invalid."));
    }
    Ok(value.to_owned())
}

fn required_id(value: Option<&str>, label: &str, prefix: &str) -> Result<String, String> {
    let value = value
        .ok_or_else(|| format!("A {label} is required."))?
        .trim();
    if value.len() > 128
        || !value.starts_with(prefix)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        return Err(format!("The {label} is invalid."));
    }
    Ok(value.to_owned())
}

fn safe_text(value: Option<&str>, label: &str, maximum: usize) -> Result<String, String> {
    let value = value
        .ok_or_else(|| format!("A {label} is required."))?
        .trim();
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character.is_control())
    {
        return Err(format!(
            "The {label} must be printable and at most {maximum} bytes."
        ));
    }
    Ok(value.to_owned())
}

fn project_file(workspace: &Path, value: &str) -> Result<PathBuf, String> {
    let relative = Path::new(value.trim());
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(
            "Artifact paths must be project-relative and may not escape the workspace.".into(),
        );
    }
    let candidate = std::fs::canonicalize(workspace.join(relative))
        .map_err(|error| format!("Could not open the project artifact: {error}"))?;
    if !candidate.starts_with(workspace) || !candidate.is_file() {
        return Err("The artifact must be a file inside the selected workspace.".into());
    }
    Ok(candidate)
}

fn workspace_binding(workspace: &Path) -> Result<Option<SessionBinding>, String> {
    let (manifest, _) = ProjectManifest::load(workspace).map_err(|report| {
        report
            .errors
            .into_iter()
            .map(|issue| format!("{} {}: {}", issue.code, issue.path, issue.message))
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let Some(place_key) = manifest.project.default_place.as_ref() else {
        return Ok(None);
    };
    let Some(channel) = manifest.project.default_art_channel.as_ref() else {
        return Ok(None);
    };
    let place = manifest
        .places
        .get(place_key)
        .ok_or_else(|| "The default place is not present in the validated manifest.".to_owned())?;
    let project_id = std::fs::read_to_string(workspace.join(".neuman/project-id"))
        .map_err(|error| format!("Could not read the local project identity: {error}"))?;
    let project_id = project_id.trim().parse::<ProjectId>().map_err(|_| {
        "The local project identity is invalid; run the project initializer again.".to_owned()
    })?;
    let config_hash = hash_canonical("neuman-project-manifest-v1\0", &manifest)
        .map_err(|error| error.to_string())?;
    let ownership_hash = hash_canonical("neuman-ownership-v1\0", &place.ownership)
        .map_err(|error| error.to_string())?;
    let policy = manifest
        .policies
        .get(&place.release_policy)
        .ok_or_else(|| "The selected place release policy is missing.".to_owned())?;
    let policy_hash =
        hash_canonical("neuman-policy-v1\0", policy).map_err(|error| error.to_string())?;
    let context_version = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "The system clock is invalid.".to_owned())?
        .as_millis()
        .try_into()
        .map_err(|_| "The system clock value is out of range.".to_owned())?;
    let accepted_head = LocalStudioOrchestrator::open(workspace)
        .map_err(|error| format!("{}: {}", error.code, error.message))?
        .accepted_head()
        .map_err(|error| format!("{}: {}", error.code, error.message))?;

    Ok(Some(SessionBinding {
        project_id: project_id.to_string(),
        workspace_id: WorkspaceId::new().to_string(),
        universe_id: place
            .authoring
            .as_ref()
            .map(|target| target.universe_id.to_string()),
        place_id: place
            .authoring
            .as_ref()
            .map(|target| target.place_id.to_string()),
        channel: channel.clone(),
        config_hash: config_hash.to_string(),
        ownership_hash: ownership_hash.to_string(),
        policy_hash: policy_hash.to_string(),
        accepted_art_head: accepted_head
            .as_ref()
            .map(|revision| revision.art_revision_id.to_string()),
        accepted_state_root: accepted_head
            .as_ref()
            .map(|revision| revision.state_root_hash.to_string()),
        context_version,
    }))
}

fn operation_summary(action: &str, succeeded: bool, stdout: &str, stderr: &str) -> String {
    if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(message) = envelope
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
        {
            return message.to_owned();
        }
        if succeeded {
            let identifier = envelope
                .pointer("/result/bundleHash")
                .or_else(|| envelope.pointer("/result/revisionId"))
                .or_else(|| envelope.pointer("/result/releaseId"))
                .and_then(serde_json::Value::as_str);
            if let Some(identifier) = identifier {
                return format!("{} completed: {identifier}", action.replace('-', " "));
            }
        }
    }
    if succeeded {
        format!("{} completed successfully.", action.replace('-', " "))
    } else {
        let diagnostic = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("No state was recorded.");
        format!("Operation failed: {diagnostic}")
    }
}

fn cli_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("NEUMAN_CLI") {
        return Some(PathBuf::from(path));
    }
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name(if cfg!(windows) {
        "neuman.exe"
    } else {
        "neuman"
    });
    Some(path)
}

fn random_url_token(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn open_system_browser(url: &str) -> Result<(), String> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .status()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err("The system browser could not be opened.".into()),
        Err(error) => Err(format!("The system browser could not be opened: {error}")),
    }
}

async fn start_embedded_bridge(state: DesktopState) {
    let mut config = BridgeConfig::default();
    if let Some(directories) = directories::ProjectDirs::from("dev", "NeuMan", "NeuMan") {
        config.transfer_dir = directories.data_local_dir().join("studio-transfers");
    }
    let running = match BridgeService::start(config).await {
        Ok(running) => running,
        Err(error) => {
            state.bridge_ui.lock().await.public.last_error = Some(error.to_string());
            return;
        }
    };
    let handle = running.handle.clone();
    handle
        .set_orchestrator(Arc::new(DesktopStudioOrchestrator {
            state: state.clone(),
        }))
        .await;
    let mut events = handle.subscribe();
    {
        let mut ui = state.bridge_ui.lock().await;
        ui.public.available = true;
        ui.public.discovery_address = Some(running.discovery_addr.to_string());
        ui.public.session_address = Some(running.session_addr.to_string());
        ui.public.last_error = None;
    }
    *state.bridge_handle.lock().await = Some(handle);
    *state.bridge_runtime.lock().await = Some(running);

    loop {
        match events.recv().await {
            Ok(event) => record_bridge_event(&state, event).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                state.bridge_ui.lock().await.public.last_error =
                    Some("The desktop missed bridge status events; reconcile Studio sessions before mutation.".into());
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    let mut ui = state.bridge_ui.lock().await;
    ui.public.available = false;
    ui.public.session_count = 0;
    ui.sessions.clear();
}

async fn record_bridge_event(state: &DesktopState, event: BridgeEvent) {
    let mut ui = state.bridge_ui.lock().await;
    match event {
        BridgeEvent::PairingChallengeIssued {
            challenge,
            pairing_code,
            ..
        } => {
            ui.challenge_codes.insert(challenge, pairing_code);
        }
        BridgeEvent::PairingRequested {
            challenge,
            plugin_installation_id,
            studio_user_id,
            studio_version,
            studio_platform,
        } => {
            let pairing_code = ui
                .challenge_codes
                .get(&challenge)
                .cloned()
                .unwrap_or_else(|| "------".into());
            ui.public.pending_pairings.retain(|prompt| {
                prompt.challenge != challenge
                    || prompt.plugin_installation_id != plugin_installation_id
            });
            ui.public.pending_pairings.push(PairingPrompt {
                challenge,
                pairing_code,
                plugin_installation_id,
                studio_user_id,
                studio_version,
                studio_platform,
            });
        }
        BridgeEvent::Paired {
            plugin_installation_id,
        } => {
            let removed = ui
                .public
                .pending_pairings
                .iter()
                .filter(|prompt| prompt.plugin_installation_id == plugin_installation_id)
                .map(|prompt| prompt.challenge.clone())
                .collect::<Vec<_>>();
            ui.public
                .pending_pairings
                .retain(|prompt| prompt.plugin_installation_id != plugin_installation_id);
            for challenge in removed {
                ui.challenge_codes.remove(&challenge);
            }
        }
        BridgeEvent::SessionConnected { session_id, .. } => {
            ui.sessions.insert(session_id);
            ui.public.session_count = ui.sessions.len();
        }
        BridgeEvent::SessionDisconnected { session_id } => {
            ui.sessions.remove(&session_id);
            ui.public.session_count = ui.sessions.len();
        }
        BridgeEvent::ProtocolViolation { code, .. } => {
            ui.public.last_error = Some(format!("Studio bridge rejected a message: {code}"));
        }
        BridgeEvent::StudioEvent { .. }
        | BridgeEvent::CaptureProposal { .. }
        | BridgeEvent::ApplyReceipt { .. }
        | BridgeEvent::TransferVerified { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_no_entry_is_signed_out_but_access_failure_is_not() {
        let absent = oauth_status_from_vault(VaultRestore::NoEntry);
        assert!(matches!(absent.phase, OAuthPhase::SignedOut));
        let failed = oauth_status_from_vault(VaultRestore::Failed);
        assert!(matches!(failed.phase, OAuthPhase::Failed));
        assert!(
            failed
                .message
                .as_deref()
                .unwrap()
                .contains("fallback store")
        );
    }

    #[test]
    fn corrupt_vault_json_fails_closed() {
        let status = oauth_status_from_vault(VaultRestore::Secret("not-json".into()));
        assert!(matches!(status.phase, OAuthPhase::Failed));
        assert!(status.account_id.is_none());
        assert!(status.message.as_deref().unwrap().contains("invalid"));
    }

    fn stored_oauth_fixture() -> StoredOAuth {
        StoredOAuth {
            schema_version: 1,
            client_id: COMPILED_OAUTH_CLIENT_ID
                .unwrap_or("public-client-123")
                .into(),
            stored_at_unix_seconds: 1_000,
            tokens: OAuthTokenSet {
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                id_token: "header.payload.signature".into(),
                token_type: "Bearer".into(),
                expires_in: 900,
                scope: "openid profile".into(),
            },
            user: UserInfo {
                sub: "42".into(),
                name: Some("Builder".into()),
                preferred_username: None,
            },
        }
    }

    #[test]
    fn vault_restore_validates_schema_scope_clock_and_expiry_state() {
        let stored = stored_oauth_fixture();
        let active = oauth_status_from_vault_at(
            VaultRestore::Secret(serde_json::to_string(&stored).unwrap()),
            1_100,
        );
        assert!(matches!(active.phase, OAuthPhase::SignedIn));
        let expired = oauth_status_from_vault_at(
            VaultRestore::Secret(serde_json::to_string(&stored).unwrap()),
            2_000,
        );
        assert!(matches!(expired.phase, OAuthPhase::SignedIn));
        assert!(expired.message.as_deref().unwrap().contains("rotate"));

        let mut invalid = stored;
        invalid.schema_version = 2;
        let failed = oauth_status_from_vault_at(
            VaultRestore::Secret(serde_json::to_string(&invalid).unwrap()),
            1_100,
        );
        assert!(matches!(failed.phase, OAuthPhase::Failed));
        invalid.schema_version = 1;
        invalid.tokens.scope = "openid".into();
        let failed = oauth_status_from_vault_at(
            VaultRestore::Secret(serde_json::to_string(&invalid).unwrap()),
            1_100,
        );
        assert!(matches!(failed.phase, OAuthPhase::Failed));
    }

    #[test]
    fn initial_tokens_require_every_fixed_scope_and_nonempty_secrets() {
        let mut tokens = OAuthTokenSet {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            id_token: "header.payload.signature".into(),
            token_type: "Bearer".into(),
            expires_in: 900,
            scope: "profile openid".into(),
        };
        validate_initial_token_set(&tokens).unwrap();
        tokens.scope = "openid".into();
        assert!(
            validate_initial_token_set(&tokens)
                .unwrap_err()
                .contains("scope downgrade")
        );
        tokens.scope = "openid profile".into();
        tokens.refresh_token.clear();
        assert!(
            validate_initial_token_set(&tokens)
                .unwrap_err()
                .contains("malformed token")
        );
    }

    #[test]
    fn discovery_endpoints_are_exact_not_merely_same_origin() {
        let mut discovery = DiscoveryDocument {
            issuer: OAUTH_ISSUER.into(),
            token_endpoint: OAUTH_TOKEN_ENDPOINT.into(),
            userinfo_endpoint: OAUTH_USERINFO_ENDPOINT.into(),
            jwks_uri: OAUTH_JWKS_ENDPOINT.into(),
        };
        validate_discovery(&discovery).unwrap();
        discovery.token_endpoint = "https://apis.roblox.com/redirect-me".into();
        assert!(validate_discovery(&discovery).is_err());
    }
}

fn main() {
    let state = DesktopState::default();
    let bridge_state = state.clone();
    let builder = tauri::Builder::default();
    // Source/development builds intentionally omit the official updater key.
    // Registering the updater without that key makes Tauri fail at startup;
    // official release preflight requires a real embedded key and therefore
    // takes this branch in signed builds.
    let builder = if updater_public_key_is_embedded() {
        builder.plugin(tauri_plugin_updater::Builder::new().build())
    } else {
        builder
    };
    builder
        .manage(state)
        .setup(move |_| {
            tauri::async_runtime::spawn(start_embedded_bridge(bridge_state.clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            system_status,
            oauth_status,
            refresh_roblox_oauth,
            roblox_resource_status,
            refresh_roblox_resources,
            probe_roblox_universe,
            select_roblox_place,
            clear_roblox_resource_selection,
            oauth_client_configuration,
            bridge_status,
            approve_studio_pairing,
            start_roblox_oauth,
            cancel_roblox_oauth,
            logout_roblox,
            execute_action
        ])
        .run(tauri::generate_context!())
        .expect("NeuMan desktop runtime failed");
}

fn updater_public_key_is_embedded() -> bool {
    serde_json::from_str::<serde_json::Value>(include_str!("tauri.conf.json"))
        .ok()
        .and_then(|config| {
            config
                .pointer("/plugins/updater/pubkey")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .map(str::to_owned)
        })
        .is_some_and(|key| !key.is_empty())
}
