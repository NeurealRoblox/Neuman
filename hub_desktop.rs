//! Native-only adapter between one selected workspace, a self-hosted Hub, and
//! the authenticated loopback Studio bridge.
//!
//! The renderer and Studio plugin never receive the Hub bearer. The endpoint is
//! taken only from `NEUMAN_HUB_URL` at process startup and must exactly match the
//! credential-free project declaration.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Method, Response, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

use crate::{
    bridge::{BridgeHandle, IncomingCell, IncomingRevision, SessionBinding},
    core::{
        HubAcceptedRevisionRequest, LocalStudioOrchestrator, ProjectManifest, StudioAcceptedCell,
    },
    domain::{ArtRevision, CellId, ContentHash},
    hub::{
        AcquireLeaseBatchRequest, ArtProposal, ArtRevision as HubArtRevision,
        CanonicalArtCell as HubManifestCell, CanonicalArtManifest as HubArtManifest,
        CompleteUploadRequest, CreateArtProposalRequest, DownloadNegotiation, EventEnvelope,
        EventPage, LeaseBatch, NegotiateUploadRequest, ObjectMetadata, Project as HubProject,
        UploadNegotiation,
    },
};

const MANIFEST_SCHEMA: &str = "dev.neuman.hub-art-manifest/v1";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.neuman.art-manifest+json";
const CELL_MEDIA_TYPE: &str = "application/x-roblox-rbxm";
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_CELL_BYTES: usize = 96 * 1024 * 1024;
const MAX_CELLS: usize = 128;
const HUB_KEYRING_SERVICE: &str = "dev.neuman.manager.hub";

/// Fail-closed Hub adapter errors. Messages never contain credentials or bytes.
#[derive(Debug, thiserror::Error)]
#[allow(
    missing_docs,
    reason = "variants are stable fail-closed adapter error categories"
)]
pub enum HubDesktopError {
    #[error("HUB_DESKTOP_CONFIG_INVALID: {0}")]
    Config(String),
    #[error("HUB_DESKTOP_CREDENTIAL_UNAVAILABLE")]
    CredentialUnavailable,
    #[error("HUB_DESKTOP_TRANSPORT_FAILED: {0}")]
    Transport(String),
    #[error("HUB_DESKTOP_PROTOCOL_INVALID: {0}")]
    Protocol(String),
    #[error("HUB_DESKTOP_CONTEXT_STALE")]
    ContextStale,
    #[error("HUB_DESKTOP_CURSOR_EXPIRED")]
    CursorExpired,
    #[error("HUB_DESKTOP_LOCAL_STATE_FAILED: {0}")]
    LocalState(String),
}

type Result<T> = std::result::Result<T, HubDesktopError>;

/// Credential-free project declaration for the optional Hub provider.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HubProviderDeclaration {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    project_id: String,
}

/// Exact native configuration fixed for one selected workspace generation.
#[derive(Clone, Debug)]
pub struct HubDesktopConfig {
    workspace: PathBuf,
    endpoint: Url,
    remote_project_id: String,
    local_project_id: String,
    local_channel: String,
    context_version: u64,
    authority_id: String,
}

impl HubDesktopConfig {
    /// Resolves an adapter only when both a credential-free project declaration
    /// and the trusted startup endpoint exist. The manifest URL is a cross-check,
    /// never the source of the network destination.
    pub fn from_workspace_environment(
        workspace: &Path,
        binding: &SessionBinding,
    ) -> Result<Option<Self>> {
        let (manifest, _) = ProjectManifest::load(workspace).map_err(|report| {
            HubDesktopError::Config(
                report
                    .errors
                    .into_iter()
                    .map(|issue| issue.code)
                    .collect::<Vec<_>>()
                    .join(","),
            )
        })?;
        let Some(raw) = manifest.providers.hub else {
            return Ok(None);
        };
        let declaration: HubProviderDeclaration = serde_json::from_value(raw)
            .map_err(|_| HubDesktopError::Config("invalid providers.hub declaration".into()))?;
        if declaration.kind != "neuman-hub" {
            return Err(HubDesktopError::Config(
                "providers.hub.type must be neuman-hub".into(),
            ));
        }
        validate_scoped_id(&declaration.project_id, "prj_")?;
        let startup = match std::env::var("NEUMAN_HUB_URL") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => {
                return Err(HubDesktopError::Config(
                    "providers.hub requires trusted NEUMAN_HUB_URL startup configuration".into(),
                ));
            }
            Err(_) => {
                return Err(HubDesktopError::Config(
                    "NEUMAN_HUB_URL is not valid Unicode".into(),
                ));
            }
        };
        let endpoint = validate_endpoint(&startup)?;
        let declared = validate_endpoint(&declaration.url)?;
        if endpoint != declared {
            return Err(HubDesktopError::Config(
                "NEUMAN_HUB_URL does not match the project declaration".into(),
            ));
        }
        let authority_id = authority_id(&endpoint, &declaration.project_id);
        Ok(Some(Self {
            workspace: workspace.to_owned(),
            endpoint,
            remote_project_id: declaration.project_id,
            local_project_id: binding.project_id.clone(),
            local_channel: binding.channel.clone(),
            context_version: binding.context_version,
            authority_id,
        }))
    }

    /// Stable OS-vault account derived only from non-secret authority metadata.
    pub fn credential_account(&self) -> String {
        vault_account(&self.endpoint, &self.remote_project_id)
    }
}

/// Stores a Hub bearer in the OS credential vault for a future native auth or
/// administrator provisioning flow. This function is never exposed to Tauri.
pub fn provision_hub_bearer(endpoint: &str, project_id: &str, bearer: &str) -> Result<()> {
    let endpoint = validate_endpoint(endpoint)?;
    validate_scoped_id(project_id, "prj_")?;
    validate_bearer(bearer)?;
    keyring::Entry::new(HUB_KEYRING_SERVICE, &vault_account(&endpoint, project_id))
        .map_err(|_| HubDesktopError::CredentialUnavailable)?
        .set_password(bearer)
        .map_err(|_| HubDesktopError::CredentialUnavailable)
}

struct HubBearer(String);

impl HubBearer {
    fn load(config: &HubDesktopConfig) -> Result<Self> {
        let entry = keyring::Entry::new(HUB_KEYRING_SERVICE, &config.credential_account())
            .map_err(|_| HubDesktopError::CredentialUnavailable)?;
        let value = entry
            .get_password()
            .map_err(|_| HubDesktopError::CredentialUnavailable)?;
        validate_bearer(&value)?;
        Ok(Self(value))
    }
}

/// Full canonical state snapshot carried as a Hub CAS object. Proposals lock
/// only `changed_cell_ids`; full cells make expired-cursor recovery possible.
/// One locally durable capture submitted to Hub under idempotent CAS semantics.
#[derive(Clone, Debug)]
pub struct HubCapture {
    /// Exact authenticated loopback Studio session that originated the delta.
    pub source_session_id: String,
    /// Complete locally durable art revision.
    pub revision: ArtRevision,
    /// Accepted local root on which this capture was based.
    pub base_state_root: Option<ContentHash>,
    /// Cells changed by this capture; Hub derives leases from this set.
    pub changed_cell_ids: Vec<CellId>,
    /// Full state materialization used for cursor-recovery snapshots.
    pub cells: Vec<StudioAcceptedCell>,
}

/// Native Hub adapter. The bearer is private and its type has no `Debug` impl.
pub struct HubDesktopAdapter {
    config: HubDesktopConfig,
    bearer: HubBearer,
    session_id: String,
    client: reqwest::Client,
    bridge: BridgeHandle,
}

impl std::fmt::Debug for HubDesktopAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HubDesktopAdapter")
            .field("endpoint", &self.config.endpoint)
            .field("remote_project_id", &self.config.remote_project_id)
            .finish_non_exhaustive()
    }
}

impl HubDesktopAdapter {
    /// Creates the adapter from an already fixed workspace generation and its
    /// OS-protected bearer.
    pub fn open(config: HubDesktopConfig, bridge: BridgeHandle) -> Result<Self> {
        let bearer = HubBearer::load(&config)?;
        Self::with_bearer(config, bridge, bearer)
    }

    fn with_bearer(
        config: HubDesktopConfig,
        bridge: BridgeHandle,
        bearer: HubBearer,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .user_agent(format!("neuman-desktop/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| HubDesktopError::Transport("could not build HTTP client".into()))?;
        Ok(Self {
            config,
            bearer,
            session_id: format!("desktop-{}", Uuid::new_v4()),
            client,
            bridge,
        })
    }

    /// Uploads a full immutable state manifest and referenced native cells, then
    /// creates one Hub proposal. Every mutation uses deterministic idempotency.
    pub async fn publish_capture(&self, capture: HubCapture) -> Result<ArtProposal> {
        validate_scoped_id(&capture.source_session_id, "ses_")?;
        if capture.revision.state_root_hash
            != ArtRevision::compute_state_root(&capture.revision.cells)
                .map_err(|_| HubDesktopError::Protocol("invalid local revision state".into()))?
        {
            return Err(HubDesktopError::Protocol(
                "local revision state root is invalid".into(),
            ));
        }
        let total = validate_local_cells(&capture.cells, &capture.revision)?;
        if total > MAX_TOTAL_CELL_BYTES {
            return Err(HubDesktopError::Protocol(
                "Hub snapshot exceeds the native-cell limit".into(),
            ));
        }
        let project: HubProject = self
            .get_json(&format!(
                "/api/v1/projects/{}",
                self.config.remote_project_id
            ))
            .await?;
        if project.id != self.config.remote_project_id {
            return Err(HubDesktopError::Protocol(
                "Hub project identity mismatch".into(),
            ));
        }
        let head: Value = self
            .get_json(&format!(
                "/api/v1/projects/{}/art-channels/{}/head",
                self.config.remote_project_id, project.default_channel_id
            ))
            .await?;
        let base_hub_revision_id = head
            .get("headRevisionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        for cell in &capture.cells {
            self.ensure_object(
                &cell.content_hash.to_string(),
                &cell.bytes,
                CELL_MEDIA_TYPE,
                &capture.revision.art_revision_id.to_string(),
            )
            .await?;
        }
        let changed: BTreeSet<String> = capture
            .changed_cell_ids
            .iter()
            .map(ToString::to_string)
            .collect();
        if changed.is_empty()
            || changed.iter().any(|id| {
                !capture
                    .revision
                    .cells
                    .keys()
                    .any(|cell| cell.to_string() == *id)
            })
        {
            return Err(HubDesktopError::Protocol(
                "changed cells are not a non-empty subset of the revision".into(),
            ));
        }
        let cell_hashes = capture
            .revision
            .cells
            .iter()
            .filter(|(cell_id, _)| changed.contains(&cell_id.to_string()))
            .map(|(cell_id, state)| (cell_id.to_string(), state.snapshot_hash.to_string()))
            .collect();
        let _: LeaseBatch = self
            .send_json(
                Method::POST,
                &format!(
                    "/api/v1/projects/{}/locks:acquireBatch",
                    self.config.remote_project_id
                ),
                Some(json!(AcquireLeaseBatchRequest {
                    channel_id: project.default_channel_id.clone(),
                    resource_ids: changed.iter().cloned().collect(),
                    base_revision_id: base_hub_revision_id.clone(),
                    workstream: "desktop-studio-capture".into(),
                    intended_action: "art-proposal".into(),
                    cell_hashes,
                })),
                Some(idempotency_key(
                    "lease",
                    &capture.revision.art_revision_id.to_string(),
                )),
            )
            .await?;
        let manifest = HubArtManifest {
            schema_version: MANIFEST_SCHEMA.into(),
            project_id: self.config.remote_project_id.clone(),
            channel_id: project.default_channel_id.clone(),
            base_hub_revision_id: base_hub_revision_id.clone(),
            base_state_root: capture.base_state_root.map(|value| value.to_string()),
            state_root: capture.revision.state_root_hash.to_string(),
            source_session_id: capture.source_session_id,
            local_revision_id: capture.revision.art_revision_id.to_string(),
            changed_cell_ids: changed.iter().cloned().collect(),
            cells: capture
                .cells
                .iter()
                .map(|cell| HubManifestCell {
                    cell_id: cell.cell_id.to_string(),
                    parent_path: cell.slot_path.clone(),
                    content_hash: cell.content_hash.to_string(),
                    size_bytes: cell.bytes.len() as u64,
                })
                .collect(),
        };
        let manifest_bytes = serde_jcs::to_vec(&manifest)
            .map_err(|_| HubDesktopError::Protocol("manifest serialization failed".into()))?;
        let manifest_hash = ContentHash::digest(&manifest_bytes).to_string();
        self.ensure_object(
            &manifest_hash,
            &manifest_bytes,
            MANIFEST_MEDIA_TYPE,
            &capture.revision.art_revision_id.to_string(),
        )
        .await?;
        let mut object_hashes = manifest
            .cells
            .iter()
            .map(|cell| cell.content_hash.clone())
            .collect::<Vec<_>>();
        object_hashes.push(manifest_hash.clone());
        object_hashes.sort();
        object_hashes.dedup();
        let request = CreateArtProposalRequest {
            channel_id: project.default_channel_id,
            base_revision_id: base_hub_revision_id,
            state_hash: capture.revision.state_root_hash.to_string(),
            title: format!("Studio checkpoint {}", capture.revision.art_revision_id),
            description: capture.revision.message,
            resource_ids: changed.into_iter().collect(),
            object_hashes,
        };
        self.send_json(
            Method::POST,
            &format!(
                "/api/v1/projects/{}/art-proposals",
                self.config.remote_project_id
            ),
            Some(json!(request)),
            Some(idempotency_key(
                "proposal",
                &capture.revision.art_revision_id.to_string(),
            )),
        )
        .await
    }

    /// Maintains authenticated at-least-once event delivery until shutdown.
    /// Retries are bounded and cursor advancement follows successful local CAS,
    /// ledger import, and bridge fan-out only.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut retry = 0_u32;
        let mut recovery_mode = false;
        loop {
            if *shutdown.borrow() {
                return;
            }
            match self.poll_events_once(recovery_mode).await {
                Ok(event_count) => {
                    retry = 0;
                    if event_count < 200 {
                        recovery_mode = false;
                    }
                    let delay = if event_count == 0 {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_millis(10)
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {},
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() { return; }
                        }
                    }
                    continue;
                }
                Err(HubDesktopError::CursorExpired) => {
                    if self
                        .local_orchestrator()
                        .and_then(|local| {
                            local
                                .clear_hub_stream_cursor(&self.config.authority_id)
                                .map_err(local_error)
                        })
                        .is_ok()
                    {
                        recovery_mode = true;
                        retry = 0;
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "self-hosted Hub stream disconnected");
                }
            }
            let delay = Duration::from_secs(1_u64 << retry.min(5));
            retry = retry.saturating_add(1);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return; }
                }
            }
        }
    }

    async fn poll_events_once(&self, recovery_mode: bool) -> Result<usize> {
        let local = self.local_orchestrator()?;
        let cursor = local
            .hub_stream_cursor(&self.config.authority_id)
            .map_err(local_error)?
            .map(|(_, cursor)| cursor);
        let mut url = api_url(
            &self.config.endpoint,
            &format!("/api/v1/projects/{}/events", self.config.remote_project_id),
        )?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", "200");
            if let Some(cursor) = cursor {
                query.append_pair("cursor", &cursor);
            }
        }
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.bearer.0)
            .header("x-neuman-session-id", &self.session_id)
            .send()
            .await
            .map_err(|_| HubDesktopError::Transport("Hub event request failed".into()))?;
        if response.status() == StatusCode::GONE {
            return Err(HubDesktopError::CursorExpired);
        }
        require_success(response.status())?;
        let bytes = read_bounded(response, MAX_JSON_BYTES).await?;
        let page: EventPage = serde_json::from_slice(&bytes)
            .map_err(|_| HubDesktopError::Protocol("Hub event page is invalid".into()))?;
        let count = page.events.len();
        for event in &page.events {
            self.process_event(event, recovery_mode).await?;
            local
                .put_hub_stream_cursor(&self.config.authority_id, event.sequence, &event.cursor)
                .map_err(local_error)?;
        }
        if page.next_cursor.as_ref().is_some_and(|cursor| {
            page.events
                .last()
                .is_none_or(|event| &event.cursor != cursor)
        }) {
            return Err(HubDesktopError::Protocol(
                "Hub event page cursor is inconsistent".into(),
            ));
        }
        Ok(count)
    }

    async fn process_event(&self, event: &EventEnvelope, recovery_mode: bool) -> Result<()> {
        if event.project_id != self.config.remote_project_id {
            return Err(HubDesktopError::Protocol(
                "event crossed the configured Hub project boundary".into(),
            ));
        }
        if event.event_type != "art.channel.head_changed" {
            return Ok(());
        }
        let revision_id = event
            .payload
            .get("revisionId")
            .and_then(Value::as_str)
            .ok_or_else(|| HubDesktopError::Protocol("head event has no revisionId".into()))?;
        validate_scoped_id(revision_id, "arev_")?;
        let revision: HubArtRevision = self
            .get_json(&format!(
                "/api/v1/projects/{}/art-revisions/{revision_id}",
                self.config.remote_project_id
            ))
            .await?;
        if revision.id != revision_id || revision.project_id != self.config.remote_project_id {
            return Err(HubDesktopError::Protocol(
                "Hub revision identity mismatch".into(),
            ));
        }
        if event.payload.get("stateHash").and_then(Value::as_str)
            != Some(revision.state_hash.as_str())
        {
            return Err(HubDesktopError::Protocol(
                "Hub event and revision state roots differ".into(),
            ));
        }
        let manifest: HubArtManifest = serde_json::from_value(revision.state.clone())
            .map_err(|_| HubDesktopError::Protocol("invalid Hub art manifest".into()))?;
        validate_manifest(&manifest, &revision)?;
        let project: HubProject = self
            .get_json(&format!(
                "/api/v1/projects/{}",
                self.config.remote_project_id
            ))
            .await?;
        if manifest.channel_id != project.default_channel_id
            || revision.channel_id != project.default_channel_id
        {
            return Err(HubDesktopError::Protocol(
                "Hub revision is outside the configured default channel".into(),
            ));
        }
        let local = self.local_orchestrator()?;
        let local_head = local.accepted_head().map_err(local_error)?;
        let local_root = local_head
            .as_ref()
            .map(|value| value.state_root_hash.to_string());
        let replace_state = recovery_mode || local_root != manifest.base_state_root;
        let selected: BTreeSet<&str> = if replace_state {
            manifest
                .cells
                .iter()
                .map(|cell| cell.cell_id.as_str())
                .collect()
        } else {
            manifest
                .changed_cell_ids
                .iter()
                .map(String::as_str)
                .collect()
        };
        let mut changed_cells = Vec::with_capacity(selected.len());
        let mut total = 0_usize;
        for cell in manifest
            .cells
            .iter()
            .filter(|cell| selected.contains(cell.cell_id.as_str()))
        {
            let limit = usize::try_from(cell.size_bytes)
                .ok()
                .filter(|size| *size <= MAX_TOTAL_CELL_BYTES)
                .ok_or_else(|| HubDesktopError::Protocol("Hub cell size is invalid".into()))?;
            total = total
                .checked_add(limit)
                .filter(|total| *total <= MAX_TOTAL_CELL_BYTES)
                .ok_or_else(|| {
                    HubDesktopError::Protocol("Hub cell snapshot exceeds limit".into())
                })?;
            let bytes = self.download_object(&cell.content_hash, limit).await?;
            if bytes.len() != limit || ContentHash::digest(&bytes).to_string() != cell.content_hash
            {
                return Err(HubDesktopError::Protocol("Hub cell hash mismatch".into()));
            }
            changed_cells.push(StudioAcceptedCell {
                cell_id: cell
                    .cell_id
                    .parse()
                    .map_err(|_| HubDesktopError::Protocol("invalid Hub cell ID".into()))?,
                slot_path: cell.parent_path.clone(),
                content_hash: cell
                    .content_hash
                    .parse()
                    .map_err(|_| HubDesktopError::Protocol("invalid Hub cell hash".into()))?,
                bytes,
            });
        }
        if changed_cells.len() != selected.len() {
            return Err(HubDesktopError::Protocol(
                "Hub manifest changed-cell set is incomplete".into(),
            ));
        }
        let outcome = local
            .import_hub_accepted_revision(HubAcceptedRevisionRequest {
                remote_authority_id: self.config.authority_id.clone(),
                event_id: event.id.clone(),
                remote_revision_id: revision.id.clone(),
                base_state_root: manifest
                    .base_state_root
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|_| HubDesktopError::Protocol("invalid Hub base state root".into()))?,
                state_root: manifest
                    .state_root
                    .parse()
                    .map_err(|_| HubDesktopError::Protocol("invalid Hub state root".into()))?,
                replace_state,
                author: format!("hub:{}", revision.created_by),
                message: format!("Accepted Hub revision {}", revision.id),
                changed_cells: changed_cells.clone(),
            })
            .map_err(local_error)?;
        if outcome.duplicate {
            return Ok(());
        }
        let incoming = IncomingRevision {
            revision_id: outcome.revision.art_revision_id.to_string(),
            state_root: outcome.revision.state_root_hash.to_string(),
            cell_ids: changed_cells
                .iter()
                .map(|cell| cell.cell_id.to_string())
                .collect(),
            author_display: format!("Hub member {}", revision.created_by),
            summary: format!("Accepted art {}", revision.id),
            cells: changed_cells
                .into_iter()
                .map(|cell| IncomingCell {
                    cell_id: cell.cell_id.to_string(),
                    parent_path: cell.slot_path,
                    content_hash: cell.content_hash.to_string(),
                    size_bytes: cell.bytes.len() as u64,
                    data: Some(URL_SAFE_NO_PAD.encode(cell.bytes)),
                    download: None,
                })
                .collect(),
        };
        self.bridge
            .fanout_remote_accepted_revision(
                &self.config.local_project_id,
                &self.config.local_channel,
                self.config.context_version,
                Some(&manifest.source_session_id),
                incoming,
            )
            .await
            .map_err(|_| HubDesktopError::ContextStale)?;
        self.bridge
            .update_accepted_head_if_context(
                &self.config.local_project_id,
                &self.config.local_channel,
                self.config.context_version,
                outcome.revision.art_revision_id.to_string(),
                outcome.revision.state_root_hash.to_string(),
            )
            .await
            .map_err(|_| HubDesktopError::ContextStale)
    }

    fn local_orchestrator(&self) -> Result<LocalStudioOrchestrator> {
        LocalStudioOrchestrator::open(&self.config.workspace).map_err(local_error)
    }

    async fn ensure_object(
        &self,
        content_hash: &str,
        bytes: &[u8],
        media_type: &str,
        referenced_by: &str,
    ) -> Result<ObjectMetadata> {
        if ContentHash::digest(bytes).to_string() != content_hash {
            return Err(HubDesktopError::Protocol(
                "outbound CAS hash mismatch".into(),
            ));
        }
        let negotiation: UploadNegotiation = self
            .send_json(
                Method::POST,
                &format!(
                    "/api/v1/projects/{}/objects:negotiateUpload",
                    self.config.remote_project_id
                ),
                Some(json!(NegotiateUploadRequest {
                    expected_hash: content_hash.into(),
                    expected_size: i64::try_from(bytes.len()).map_err(|_| {
                        HubDesktopError::Protocol("outbound object exceeds size range".into())
                    })?,
                    media_type: media_type.into(),
                })),
                Some(idempotency_key("negotiate", content_hash)),
            )
            .await?;
        if negotiation.status == "present" {
            return negotiation.object.ok_or_else(|| {
                HubDesktopError::Protocol("present object metadata missing".into())
            });
        }
        if negotiation.status != "upload_required" {
            return Err(HubDesktopError::Protocol(
                "unknown Hub upload negotiation status".into(),
            ));
        }
        let upload_id = negotiation
            .upload_id
            .ok_or_else(|| HubDesktopError::Protocol("Hub upload ID missing".into()))?;
        let upload_url = validate_transfer_url(
            &self.config.endpoint,
            negotiation
                .upload_url
                .as_deref()
                .ok_or_else(|| HubDesktopError::Protocol("Hub upload URL missing".into()))?,
            "/api/v1/transfers/uploads/",
        )?;
        let transfer_token = negotiation
            .transfer_token
            .ok_or_else(|| HubDesktopError::Protocol("Hub transfer token missing".into()))?;
        self.put_transfer(upload_url, &transfer_token, bytes)
            .await?;
        self.send_json(
            Method::POST,
            &format!(
                "/api/v1/projects/{}/uploads/{upload_id}",
                self.config.remote_project_id
            ),
            Some(json!(CompleteUploadRequest {
                purpose: "art-cell".into(),
                referenced_by: referenced_by.into(),
            })),
            Some(idempotency_key("complete", &upload_id)),
        )
        .await
    }

    async fn download_object(&self, content_hash: &str, limit: usize) -> Result<Vec<u8>> {
        let _: ContentHash = content_hash
            .parse()
            .map_err(|_| HubDesktopError::Protocol("invalid object hash".into()))?;
        let negotiation: DownloadNegotiation = self
            .get_json(&format!(
                "/api/v1/projects/{}/objects/{content_hash}:download",
                self.config.remote_project_id
            ))
            .await?;
        if negotiation.content_hash != content_hash
            || usize::try_from(negotiation.size_bytes)
                .ok()
                .is_none_or(|size| size > limit)
        {
            return Err(HubDesktopError::Protocol(
                "Hub download negotiation mismatch".into(),
            ));
        }
        let url = validate_transfer_url(
            &self.config.endpoint,
            &negotiation.download_url,
            "/api/v1/transfers/downloads/",
        )?;
        let response = self
            .client
            .get(url)
            .header("x-neuman-transfer-token", negotiation.transfer_token)
            .send()
            .await
            .map_err(|_| HubDesktopError::Transport("Hub object download failed".into()))?;
        require_success(response.status())?;
        read_bounded(response, limit).await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send_json(Method::GET, path, None, None).await
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        idempotency: Option<String>,
    ) -> Result<T> {
        let url = api_url(&self.config.endpoint, path)?;
        for attempt in 0..3_u32 {
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(&self.bearer.0)
                .header("x-neuman-session-id", &self.session_id);
            if let Some(key) = &idempotency {
                request = request.header("idempotency-key", key);
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    let bytes = read_bounded(response, MAX_JSON_BYTES).await?;
                    return serde_json::from_slice(&bytes).map_err(|_| {
                        HubDesktopError::Protocol("Hub returned invalid JSON".into())
                    });
                }
                Ok(response) if retryable_status(response.status()) && attempt < 2 => {}
                Ok(response) => return Err(status_error(response.status())),
                Err(_) if attempt < 2 => {}
                Err(_) => {
                    return Err(HubDesktopError::Transport("Hub request failed".into()));
                }
            }
            tokio::time::sleep(Duration::from_millis(200 * (1_u64 << attempt))).await;
        }
        Err(HubDesktopError::Transport("Hub request failed".into()))
    }

    async fn put_transfer(&self, url: Url, token: &str, bytes: &[u8]) -> Result<()> {
        for attempt in 0..3_u32 {
            match self
                .client
                .put(url.clone())
                .header("x-neuman-transfer-token", token)
                .body(bytes.to_vec())
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) if retryable_status(response.status()) && attempt < 2 => {}
                Ok(response) => return Err(status_error(response.status())),
                Err(_) if attempt < 2 => {}
                Err(_) => {
                    return Err(HubDesktopError::Transport("Hub upload failed".into()));
                }
            }
            tokio::time::sleep(Duration::from_millis(200 * (1_u64 << attempt))).await;
        }
        Err(HubDesktopError::Transport("Hub upload failed".into()))
    }
}

fn validate_endpoint(value: &str) -> Result<Url> {
    let mut url = Url::parse(value)
        .map_err(|_| HubDesktopError::Config("Hub endpoint is not a URL".into()))?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "http" | "https")
    {
        return Err(HubDesktopError::Config(
            "Hub endpoint contains forbidden URL components".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| HubDesktopError::Config("Hub endpoint has no host".into()))?;
    if url.scheme() == "http" && !matches!(host, "127.0.0.1" | "::1") {
        return Err(HubDesktopError::Config(
            "non-loopback Hub endpoints require HTTPS".into(),
        ));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(HubDesktopError::Config(
            "Hub endpoint must not contain a path".into(),
        ));
    }
    url.set_path("/");
    Ok(url)
}

fn validate_transfer_url(base: &Url, value: &str, prefix: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|_| HubDesktopError::Protocol("Hub transfer URL is invalid".into()))?;
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with(prefix)
    {
        return Err(HubDesktopError::Protocol(
            "Hub transfer URL escaped its trusted origin".into(),
        ));
    }
    Ok(url)
}

fn api_url(base: &Url, path: &str) -> Result<Url> {
    if !path.starts_with("/api/v1/") || path.contains("..") {
        return Err(HubDesktopError::Protocol("invalid Hub API path".into()));
    }
    base.join(path)
        .map_err(|_| HubDesktopError::Protocol("invalid Hub API URL".into()))
}

async fn read_bounded(response: Response, limit: usize) -> Result<Vec<u8>> {
    let length = response.content_length().ok_or_else(|| {
        HubDesktopError::Protocol("Hub response omitted a bounded Content-Length".into())
    })?;
    if length > limit as u64 {
        return Err(HubDesktopError::Protocol(
            "Hub response exceeds limit".into(),
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| HubDesktopError::Transport("Hub response read failed".into()))?;
    if bytes.len() != length as usize || bytes.len() > limit {
        return Err(HubDesktopError::Protocol(
            "Hub response length does not match its bound".into(),
        ));
    }
    Ok(bytes.to_vec())
}

fn validate_manifest(manifest: &HubArtManifest, revision: &HubArtRevision) -> Result<()> {
    if manifest.schema_version != MANIFEST_SCHEMA
        || manifest.project_id != revision.project_id
        || manifest.channel_id != revision.channel_id
        || manifest.base_hub_revision_id != revision.parent_revision_id
        || manifest.cells.is_empty()
        || manifest.cells.len() > MAX_CELLS
        || manifest.changed_cell_ids.is_empty()
    {
        return Err(HubDesktopError::Protocol(
            "Hub art manifest context is invalid".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut domain_state = std::collections::BTreeMap::new();
    let mut total = 0_u64;
    for cell in &manifest.cells {
        validate_scoped_id(&cell.cell_id, "cell_")?;
        let _: ContentHash = cell
            .content_hash
            .parse()
            .map_err(|_| HubDesktopError::Protocol("invalid manifest cell hash".into()))?;
        if !cell.parent_path.starts_with('/')
            || cell.parent_path.len() > 1024
            || cell.size_bytes == 0
            || !ids.insert(cell.cell_id.as_str())
        {
            return Err(HubDesktopError::Protocol(
                "invalid or duplicate manifest cell".into(),
            ));
        }
        total = total
            .checked_add(cell.size_bytes)
            .ok_or_else(|| HubDesktopError::Protocol("manifest size overflow".into()))?;
        let cell_id: CellId = cell
            .cell_id
            .parse()
            .map_err(|_| HubDesktopError::Protocol("invalid manifest cell ID".into()))?;
        domain_state.insert(
            cell_id,
            crate::domain::ArtCellState {
                cell_id,
                snapshot_hash: cell
                    .content_hash
                    .parse()
                    .map_err(|_| HubDesktopError::Protocol("invalid manifest cell hash".into()))?,
                slot_path: cell.parent_path.clone(),
            },
        );
    }
    if total > MAX_TOTAL_CELL_BYTES as u64
        || manifest
            .changed_cell_ids
            .iter()
            .any(|id| !ids.contains(id.as_str()))
    {
        return Err(HubDesktopError::Protocol(
            "manifest cell set exceeds policy".into(),
        ));
    }
    let _: ContentHash = manifest
        .state_root
        .parse()
        .map_err(|_| HubDesktopError::Protocol("invalid manifest state root".into()))?;
    let computed = ArtRevision::compute_state_root(&domain_state)
        .map_err(|_| HubDesktopError::Protocol("manifest state root failed".into()))?
        .to_string();
    if computed != manifest.state_root || revision.state_hash != manifest.state_root {
        return Err(HubDesktopError::Protocol(
            "manifest state root does not match its cells".into(),
        ));
    }
    if let Some(base) = &manifest.base_state_root {
        let _: ContentHash = base
            .parse()
            .map_err(|_| HubDesktopError::Protocol("invalid manifest base root".into()))?;
    }
    validate_scoped_id(&manifest.source_session_id, "ses_")?;
    validate_scoped_id(&manifest.local_revision_id, "art_")
}

fn validate_local_cells(cells: &[StudioAcceptedCell], revision: &ArtRevision) -> Result<usize> {
    if cells.is_empty() || cells.len() > MAX_CELLS || cells.len() != revision.cells.len() {
        return Err(HubDesktopError::Protocol(
            "local revision cell snapshot is incomplete".into(),
        ));
    }
    let mut by_id = HashMap::new();
    let mut total = 0_usize;
    for cell in cells {
        if ContentHash::digest(&cell.bytes) != cell.content_hash
            || by_id.insert(cell.cell_id, cell).is_some()
        {
            return Err(HubDesktopError::Protocol(
                "local revision cell identity is invalid".into(),
            ));
        }
        total = total.saturating_add(cell.bytes.len());
    }
    for (id, state) in &revision.cells {
        if by_id.get(id).is_none_or(|cell| {
            cell.content_hash != state.snapshot_hash || cell.slot_path != state.slot_path
        }) {
            return Err(HubDesktopError::Protocol(
                "local revision metadata does not match CAS".into(),
            ));
        }
    }
    Ok(total)
}

fn validate_scoped_id(value: &str, prefix: &str) -> Result<()> {
    if value.len() < prefix.len() + 4
        || value.len() > 256
        || !value.starts_with(prefix)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(HubDesktopError::Protocol(
            "invalid scoped identifier".into(),
        ));
    }
    Ok(())
}

fn validate_bearer(value: &str) -> Result<()> {
    if value.len() < 16 || value.len() > 4096 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(HubDesktopError::CredentialUnavailable);
    }
    Ok(())
}

fn authority_id(endpoint: &Url, project_id: &str) -> String {
    format!(
        "hub_{}",
        hex::encode(Sha256::digest(
            format!("neuman-hub-authority-v1\0{endpoint}\0{project_id}").as_bytes()
        ))
    )
}

fn vault_account(endpoint: &Url, project_id: &str) -> String {
    format!(
        "hub:{}",
        hex::encode(Sha256::digest(
            format!("neuman-hub-vault-v1\0{endpoint}\0{project_id}").as_bytes()
        ))
    )
}

fn idempotency_key(operation: &str, identity: &str) -> String {
    format!(
        "desktop-{operation}-{}",
        hex::encode(Sha256::digest(identity.as_bytes()))
    )
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn require_success(status: StatusCode) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(status_error(status))
    }
}

fn status_error(status: StatusCode) -> HubDesktopError {
    HubDesktopError::Transport(format!("Hub returned HTTP {}", status.as_u16()))
}

fn local_error(error: impl std::fmt::Display) -> HubDesktopError {
    HubDesktopError::LocalState(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_requires_tls_except_numeric_loopback_and_has_no_credentials() {
        assert!(validate_endpoint("https://hub.example.com").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:8787").is_ok());
        for invalid in [
            "http://hub.example.com",
            "https://user:pass@hub.example.com",
            "https://hub.example.com/team",
            "https://hub.example.com?token=bad",
        ] {
            assert!(validate_endpoint(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn transfer_url_cannot_escape_the_configured_hub_origin() {
        let base = validate_endpoint("https://hub.example.com").unwrap();
        assert!(
            validate_transfer_url(
                &base,
                "https://hub.example.com/api/v1/transfers/downloads/dwn_1234",
                "/api/v1/transfers/downloads/"
            )
            .is_ok()
        );
        assert!(
            validate_transfer_url(
                &base,
                "https://objects.example.com/api/v1/transfers/downloads/dwn_1234",
                "/api/v1/transfers/downloads/"
            )
            .is_err()
        );
    }

    #[test]
    fn manifest_validation_rejects_changed_cells_outside_full_snapshot() {
        let manifest = HubArtManifest {
            schema_version: MANIFEST_SCHEMA.into(),
            project_id: "prj_remote1234".into(),
            channel_id: "chn_remote1234".into(),
            base_hub_revision_id: None,
            base_state_root: None,
            state_root: ContentHash::digest(b"state").to_string(),
            source_session_id: "ses_source1234".into(),
            local_revision_id: "art_local1234".into(),
            changed_cell_ids: vec!["cell_missing1234".into()],
            cells: vec![HubManifestCell {
                cell_id: "cell_present1234".into(),
                parent_path: "/Workspace/Art".into(),
                content_hash: ContentHash::digest(b"cell").to_string(),
                size_bytes: 4,
            }],
        };
        let revision = HubArtRevision {
            id: "arev_remote1234".into(),
            project_id: manifest.project_id.clone(),
            channel_id: manifest.channel_id.clone(),
            parent_revision_id: None,
            proposal_id: "apr_remote1234".into(),
            state_hash: ContentHash::digest(b"manifest").to_string(),
            state: json!({}),
            created_by: "principal".into(),
            created_at_ms: 1,
        };
        assert!(validate_manifest(&manifest, &revision).is_err());
    }
}
