#![allow(
    missing_docs,
    reason = "REST wire models are exhaustively documented in /docs/guides/HUB_README.md and the versioned schema"
)]

//! Self-hosted NeuMan Hub reference service.
//!
//! This is intentionally a modular monolith. SQLite and the local filesystem
//! provide a runnable development vertical slice; the [`Repository`] contract
//! marks the boundary implemented by PostgreSQL/S3 in production.

use std::{
    collections::{BTreeSet, HashMap},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State, WebSocketUpgrade, ws::Message},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use futures_util::StreamExt;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::domain::{
    ArtCellState as DomainArtCellState, ArtRevision as DomainArtRevision, CellId, ContentHash,
};

#[cfg(test)]
use std::path::Path as FsPath;

const SCHEMA: &str = include_str!("schemas/hub.sql");
const API_VERSION: &str = "v1";
const LEASE_DURATION_MS: i64 = 120_000;
const LEASE_RENEWAL_TARGET_MS: i64 = 30_000;
const PRESENCE_TTL_MS: i64 = 45_000;
const TRANSFER_TTL_MS: i64 = 10 * 60_000;
const IDEMPOTENCY_TTL_MS: i64 = 24 * 60 * 60_000;
const ART_MANIFEST_SCHEMA: &str = "dev.neuman.hub-art-manifest/v1";
const ART_MANIFEST_MEDIA_TYPE: &str = "application/vnd.neuman.art-manifest+json";
#[cfg(test)]
const CELL_MEDIA_TYPE: &str = "application/x-roblox-rbxm";
const MAX_ART_MANIFEST_BYTES: usize = 1024 * 1024;

/// Canonical full art state inspected by Hub before proposal insertion.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalArtManifest {
    pub schema_version: String,
    pub project_id: String,
    pub channel_id: String,
    pub base_hub_revision_id: Option<String>,
    pub base_state_root: Option<String>,
    pub state_root: String,
    pub source_session_id: String,
    pub local_revision_id: String,
    pub changed_cell_ids: Vec<String>,
    pub cells: Vec<CanonicalArtCell>,
}

/// One cell-to-slot/content binding in a canonical Hub art state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalArtCell {
    pub cell_id: String,
    pub parent_path: String,
    pub content_hash: String,
    pub size_bytes: u64,
}

/// Runtime configuration sourced exclusively from environment variables.
#[derive(Clone, Debug)]
pub struct HubConfig {
    pub environment: String,
    pub bind: SocketAddr,
    pub database_path: PathBuf,
    pub object_dir: PathBuf,
    pub public_base_url: String,
    cursor_key: [u8; 32],
    transfer_key: [u8; 32],
    pub bootstrap_token: Option<String>,
    pub bootstrap_name: String,
    pub quotas: Quotas,
    pub event_retention: i64,
}

#[derive(Clone, Debug)]
pub struct Quotas {
    pub max_members: i64,
    pub max_active_leases: i64,
    pub max_object_bytes: i64,
    pub max_upload_bytes: usize,
}

impl HubConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let environment = env("NEUMAN_HUB_ENVIRONMENT", "development");
        let bind = env("NEUMAN_HUB_BIND", "127.0.0.1:8787")
            .parse()
            .context("NEUMAN_HUB_BIND must be a socket address")?;
        let database_path = PathBuf::from(env("NEUMAN_HUB_DATABASE", "./var/neuman-hub.sqlite3"));
        let object_dir = PathBuf::from(env("NEUMAN_HUB_OBJECT_DIR", "./var/objects"));
        let public_base_url = env("NEUMAN_HUB_PUBLIC_BASE_URL", "http://127.0.0.1:8787");
        let cursor_secret = std::env::var("NEUMAN_HUB_CURSOR_SECRET")
            .unwrap_or_else(|_| "development-cursor-secret-change-me".into());
        let bootstrap_token = std::env::var("NEUMAN_HUB_BOOTSTRAP_TOKEN").ok();
        if environment != "development" {
            anyhow::ensure!(
                public_base_url.starts_with("https://"),
                "production NEUMAN_HUB_PUBLIC_BASE_URL must use HTTPS"
            );
            anyhow::ensure!(
                cursor_secret.len() >= 32,
                "production cursor secret must be at least 32 characters"
            );
            anyhow::ensure!(
                bootstrap_token.is_none(),
                "development bootstrap tokens are forbidden outside development"
            );
        }
        let cursor_key = *blake3::hash(cursor_secret.as_bytes()).as_bytes();
        let transfer_key =
            *blake3::keyed_hash(&cursor_key, b"neuman-hub-transfer-key-v1").as_bytes();
        Ok(Self {
            environment,
            bind,
            database_path,
            object_dir,
            public_base_url,
            cursor_key,
            transfer_key,
            bootstrap_token,
            bootstrap_name: env("NEUMAN_HUB_BOOTSTRAP_NAME", "Local Administrator"),
            quotas: Quotas {
                max_members: env_parse("NEUMAN_HUB_MAX_MEMBERS_PER_PROJECT", 100)?,
                max_active_leases: env_parse("NEUMAN_HUB_MAX_ACTIVE_LEASES_PER_PROJECT", 1_000)?,
                max_object_bytes: env_parse(
                    "NEUMAN_HUB_MAX_OBJECT_BYTES_PER_PROJECT",
                    100 * 1024 * 1024 * 1024_i64,
                )?,
                max_upload_bytes: env_parse(
                    "NEUMAN_HUB_MAX_UPLOAD_BYTES",
                    512 * 1024 * 1024_usize,
                )?,
            },
            event_retention: env_parse("NEUMAN_HUB_EVENT_RETENTION", 100_000)?,
        })
    }

    #[cfg(test)]
    fn test(root: &FsPath) -> Self {
        Self {
            environment: "development".into(),
            bind: "127.0.0.1:0".parse().expect("test bind"),
            database_path: root.join("hub.sqlite3"),
            object_dir: root.join("objects"),
            public_base_url: "http://127.0.0.1:8787".into(),
            cursor_key: [7; 32],
            transfer_key: [11; 32],
            bootstrap_token: Some("test-token-with-enough-entropy".into()),
            bootstrap_name: "Test Admin".into(),
            quotas: Quotas {
                max_members: 4,
                max_active_leases: 20,
                max_object_bytes: 1024 * 1024,
                max_upload_bytes: 1024 * 1024,
            },
            event_retention: 100,
        }
    }
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}

fn env_parse<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid {name}: {error}")),
        Err(_) => Ok(default),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorDocument {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: String,
    details: Value,
}

#[derive(Debug)]
pub struct HubError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

type HubResult<T> = Result<T, HubError>;

impl HubError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: json!({}),
        }
    }

    fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "HUB_AUTHENTICATION_REQUIRED",
            "A valid bearer token is required.",
        )
    }

    fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "HUB_PERMISSION_DENIED",
            "The authenticated principal is not authorized for this project action.",
        )
    }

    fn not_found(kind: &'static str) -> Self {
        Self::new(StatusCode::NOT_FOUND, kind, "The resource was not found.")
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "HUB_INVALID_REQUEST", message)
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        error!(error = %error, "hub internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "HUB_INTERNAL",
            "The Hub could not complete the request.",
        )
    }
}

impl IntoResponse for HubError {
    fn into_response(self) -> Response {
        let status = self.status;
        let mut response = (
            status,
            Json(ErrorDocument {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    details: self.details,
                },
            }),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HubError {}

impl From<rusqlite::Error> for HubError {
    fn from(value: rusqlite::Error) -> Self {
        Self::internal(value)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub principal_id: String,
    pub display_name: String,
    pub is_operator: bool,
    pub session_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Artist,
    Developer,
    Approver,
    ReleaseManager,
    Admin,
}

#[derive(Clone, Copy)]
pub(crate) enum Permission {
    Read,
    ProposeArt,
    AcceptArt,
    Build,
    ApproveRelease,
    ExecuteRelease,
    ManageMembers,
}

impl Role {
    fn parse(value: &str) -> HubResult<Self> {
        serde_json::from_value(Value::String(value.into()))
            .map_err(|_| HubError::invalid("Unknown project role."))
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Artist => "artist",
            Self::Developer => "developer",
            Self::Approver => "approver",
            Self::ReleaseManager => "release_manager",
            Self::Admin => "admin",
        }
    }

    fn allows(self, permission: Permission) -> bool {
        match permission {
            Permission::Read => true,
            Permission::ProposeArt => matches!(self, Self::Artist | Self::Developer | Self::Admin),
            Permission::AcceptArt => matches!(self, Self::Approver | Self::Admin),
            Permission::Build => matches!(self, Self::Developer | Self::Admin),
            Permission::ApproveRelease => matches!(self, Self::Approver | Self::Admin),
            Permission::ExecuteRelease => matches!(self, Self::ReleaseManager | Self::Admin),
            Permission::ManageMembers => matches!(self, Self::Admin),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub default_channel_id: String,
    pub archived_at_ms: Option<i64>,
    pub version: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Membership {
    pub project_id: String,
    pub principal_id: String,
    pub display_name: String,
    pub role: Role,
    pub version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub id: String,
    pub sequence: i64,
    pub cursor: String,
    pub project_id: String,
    pub category: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub payload: Value,
    pub occurred_at_ms: i64,
}

#[derive(Clone)]
pub struct AppState {
    repo: Arc<SqliteRepository>,
    config: Arc<HubConfig>,
    events: broadcast::Sender<EventEnvelope>,
    presence: Arc<Mutex<HashMap<String, Presence>>>,
}

/// Repository boundary required of the production PostgreSQL adapter.
///
/// Mutations must retain the SQLite implementation's transaction semantics:
/// domain write, idempotency result, audit event, and outbox event commit as one
/// unit. Production implementations additionally use database server time and
/// row/advisory locks for lease and accepted-head serialization.
pub(crate) trait Repository: Send + Sync {
    fn readiness(&self) -> HubResult<()>;
    fn authenticate(&self, bearer_token: &str, session_id: &str) -> HubResult<Actor>;
    fn authorize_project(
        &self,
        actor: &Actor,
        project_id: &str,
        permission: Permission,
    ) -> HubResult<Role>;
}

pub struct SqliteRepository {
    connection: Mutex<Connection>,
    object_dir: PathBuf,
    quotas: Quotas,
    transfer_key: [u8; 32],
}

impl SqliteRepository {
    pub fn open(config: &HubConfig) -> anyhow::Result<Self> {
        if let Some(parent) = config.database_path.parent() {
            std::fs::create_dir_all(parent).context("create Hub database directory")?;
        }
        std::fs::create_dir_all(config.object_dir.join("tmp"))
            .context("create Hub object temp directory")?;
        std::fs::create_dir_all(config.object_dir.join("cas"))
            .context("create Hub object CAS directory")?;
        let connection = Connection::open(&config.database_path).context("open Hub database")?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .context("configure SQLite busy timeout")?;
        connection
            .execute_batch(SCHEMA)
            .context("migrate Hub schema")?;
        Ok(Self {
            connection: Mutex::new(connection),
            object_dir: config.object_dir.clone(),
            quotas: config.quotas.clone(),
            transfer_key: config.transfer_key,
        })
    }

    fn conn(&self) -> HubResult<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| HubError::internal("SQLite connection lock was poisoned"))
    }

    pub fn bootstrap_development_principal(
        &self,
        token: &str,
        display_name: &str,
    ) -> HubResult<String> {
        if token.len() < 16 {
            return Err(HubError::invalid(
                "Development bootstrap token must contain at least 16 characters.",
            ));
        }
        let token_hash = token_hash(token);
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                "SELECT principal_id FROM auth_tokens WHERE token_hash = ?1",
                [&token_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok(existing);
        }
        let principal_id = new_id("prn");
        tx.execute(
            "INSERT INTO principals(id, display_name, is_operator, created_at_ms) VALUES(?1, ?2, 1, ?3)",
            params![principal_id, display_name, now],
        )?;
        tx.execute(
            "INSERT INTO auth_tokens(token_hash, principal_id, label, created_at_ms) VALUES(?1, ?2, 'development bootstrap', ?3)",
            params![token_hash, principal_id, now],
        )?;
        tx.commit()?;
        Ok(principal_id)
    }

    fn membership_role(
        tx: &Transaction<'_>,
        actor: &Actor,
        project_id: &str,
        permission: Permission,
    ) -> HubResult<Role> {
        let role_text = tx
            .query_row(
                "SELECT m.role FROM project_memberships m JOIN projects p ON p.id=m.project_id
                 WHERE m.project_id=?1 AND m.principal_id=?2 AND p.archived_at_ms IS NULL",
                params![project_id, actor.principal_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(role_text) = role_text else {
            // Deliberately do not reveal whether the project exists.
            return Err(HubError::forbidden());
        };
        let role = Role::parse(&role_text)?;
        if !role.allows(permission) {
            return Err(HubError::forbidden());
        }
        Ok(role)
    }
}

impl Repository for SqliteRepository {
    fn readiness(&self) -> HubResult<()> {
        let conn = self.conn()?;
        let version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version != 1 {
            return Err(HubError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "HUB_UNAVAILABLE",
                "Database migrations are not current.",
            ));
        }
        if !self.object_dir.exists() {
            return Err(HubError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "HUB_UNAVAILABLE",
                "Object storage is unavailable.",
            ));
        }
        Ok(())
    }

    fn authenticate(&self, bearer_token: &str, session_id: &str) -> HubResult<Actor> {
        let hash = token_hash(bearer_token);
        let now = now_ms();
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT p.id, p.display_name, p.is_operator, t.token_hash
                 FROM auth_tokens t JOIN principals p ON p.id=t.principal_id
                 WHERE t.token_hash=?1 AND t.revoked_at_ms IS NULL
                   AND (t.expires_at_ms IS NULL OR t.expires_at_ms > ?2)",
                params![hash, now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((principal_id, display_name, is_operator, stored_hash)) = row else {
            return Err(HubError::unauthorized());
        };
        if hash.as_bytes().ct_eq(stored_hash.as_bytes()).unwrap_u8() != 1 {
            return Err(HubError::unauthorized());
        }
        Ok(Actor {
            principal_id,
            display_name,
            is_operator,
            session_id: session_id.into(),
        })
    }

    fn authorize_project(
        &self,
        actor: &Actor,
        project_id: &str,
        permission: Permission,
    ) -> HubResult<Role> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let result = Self::membership_role(&tx, actor, project_id, permission);
        tx.commit()?;
        result
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7())
}

fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

fn token_hash(token: &str) -> String {
    sha256_hex(format!("neuman-hub-development-token-v1\0{token}"))
}

fn canonical_request_hash<T: Serialize>(request: &T) -> HubResult<String> {
    serde_json::to_vec(request)
        .map(sha256_hex)
        .map_err(HubError::internal)
}

fn parse_json<T: DeserializeOwned>(value: &str) -> HubResult<T> {
    serde_json::from_str(value).map_err(HubError::internal)
}

fn authenticate(repo: &SqliteRepository, headers: &HeaderMap) -> HubResult<Actor> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(HubError::unauthorized)?;
    let token = authorization
        .strip_prefix("Bearer ")
        .ok_or_else(HubError::unauthorized)?;
    if token.is_empty() || token.len() > 4096 {
        return Err(HubError::unauthorized());
    }
    let session_id = headers
        .get("x-neuman-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_:.".contains(&byte))
        })
        .unwrap_or("api-default");
    repo.authenticate(token, session_id)
}

fn idempotency_key(headers: &HeaderMap) -> HubResult<String> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            HubError::new(
                StatusCode::BAD_REQUEST,
                "HUB_IDEMPOTENCY_REQUIRED",
                "Idempotency-Key is required for this mutation.",
            )
        })?;
    if !(8..=128).contains(&key.len())
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:.".contains(&byte))
    {
        return Err(HubError::invalid(
            "Idempotency-Key must be 8-128 URL-safe characters.",
        ));
    }
    Ok(key.into())
}

fn check_idempotency<T: DeserializeOwned>(
    tx: &Transaction<'_>,
    actor: &Actor,
    project_id: &str,
    route: &str,
    key: &str,
    request_hash: &str,
) -> HubResult<Option<T>> {
    let existing = tx
        .query_row(
            "SELECT request_hash, response_json FROM idempotency_records
             WHERE principal_id=?1 AND project_id=?2 AND route=?3 AND idempotency_key=?4",
            params![actor.principal_id, project_id, route, key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((stored_hash, response_json)) = existing {
        if stored_hash != request_hash {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_IDEMPOTENCY_CONFLICT",
                "This idempotency key was already used with a different request.",
            ));
        }
        return parse_json(&response_json).map(Some);
    }
    Ok(None)
}

fn store_idempotency<T: Serialize>(
    tx: &Transaction<'_>,
    actor: &Actor,
    project_id: &str,
    route: &str,
    key: &str,
    request_hash: &str,
    response: &T,
) -> HubResult<()> {
    let response_json = serde_json::to_string(response).map_err(HubError::internal)?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO idempotency_records(principal_id, project_id, route, idempotency_key,
         request_hash, response_status, response_json, created_at_ms, expires_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, 200, ?6, ?7, ?8)",
        params![
            actor.principal_id,
            project_id,
            route,
            key,
            request_hash,
            response_json,
            now,
            now + IDEMPOTENCY_TTL_MS
        ],
    )?;
    Ok(())
}

fn append_evidence(
    tx: &Transaction<'_>,
    actor: &Actor,
    project_id: &str,
    action: &str,
    aggregate_type: &str,
    aggregate_id: &str,
    category: &str,
    event_type: &str,
    details: &Value,
) -> HubResult<()> {
    let occurred_at = now_ms();
    let previous_hash = tx
        .query_row(
            "SELECT event_hash FROM audit_events WHERE project_id=?1 ORDER BY sequence DESC LIMIT 1",
            [project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let details_json = serde_json::to_string(details).map_err(HubError::internal)?;
    let audit_id = new_id("aud");
    let audit_material = format!(
        "neuman-audit-v1\0{}\0{audit_id}\0{project_id}\0{}\0{action}\0{aggregate_type}\0{aggregate_id}\0success\0{details_json}\0{occurred_at}",
        previous_hash.as_deref().unwrap_or(""),
        actor.principal_id
    );
    let event_hash = sha256_hex(audit_material);
    tx.execute(
        "INSERT INTO audit_events(id, project_id, actor_principal_id, action, aggregate_type,
         aggregate_id, outcome, details_json, occurred_at_ms, previous_hash, event_hash)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'success', ?7, ?8, ?9, ?10)",
        params![
            audit_id,
            project_id,
            actor.principal_id,
            action,
            aggregate_type,
            aggregate_id,
            details_json,
            occurred_at,
            previous_hash,
            event_hash
        ],
    )?;
    tx.execute(
        "INSERT INTO outbox_events(id, project_id, category, event_type, aggregate_id,
         payload_json, occurred_at_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            new_id("evt"),
            project_id,
            category,
            event_type,
            aggregate_id,
            serde_json::to_string(details).map_err(HubError::internal)?,
            occurred_at
        ],
    )?;
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectRequest {
    pub name: String,
    pub slug: String,
}

impl SqliteRepository {
    pub fn create_project(
        &self,
        actor: &Actor,
        key: &str,
        request: &CreateProjectRequest,
    ) -> HubResult<Project> {
        validate_name(&request.name, 1, 120, "Project name")?;
        validate_slug(&request.slug)?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) =
            check_idempotency(&tx, actor, "_global", "projects.create", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(existing);
        }
        let now = now_ms();
        let project = Project {
            id: new_id("prj"),
            name: request.name.trim().into(),
            slug: request.slug.clone(),
            default_channel_id: new_id("ach"),
            archived_at_ms: None,
            version: 1,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO projects(id, name, slug, default_channel_id, created_by,
             created_at_ms, updated_at_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                project.id,
                project.name,
                project.slug,
                project.default_channel_id,
                actor.principal_id,
                now
            ],
        )?;
        if inserted != 1 {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "A project already uses this slug.",
            ));
        }
        tx.execute(
            "INSERT INTO art_channels(id, project_id, name) VALUES(?1, ?2, 'accepted')",
            params![project.default_channel_id, project.id],
        )?;
        tx.execute(
            "INSERT INTO project_memberships(project_id, principal_id, role, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, 'admin', ?3, ?3)",
            params![project.id, actor.principal_id, now],
        )?;
        append_evidence(
            &tx,
            actor,
            &project.id,
            "project.created",
            "project",
            &project.id,
            "project",
            "project.created",
            &json!({"name": project.name, "slug": project.slug}),
        )?;
        store_idempotency(
            &tx,
            actor,
            "_global",
            "projects.create",
            key,
            &request_hash,
            &project,
        )?;
        tx.commit()?;
        Ok(project)
    }

    pub fn list_projects(&self, actor: &Actor) -> HubResult<Vec<Project>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT p.id,p.name,p.slug,p.default_channel_id,p.archived_at_ms,p.version,
             p.created_at_ms,p.updated_at_ms FROM projects p
             JOIN project_memberships m ON m.project_id=p.id
             WHERE m.principal_id=?1 ORDER BY p.name,p.id LIMIT 201",
        )?;
        let rows = statement.query_map([&actor.principal_id], project_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_project(&self, actor: &Actor, project_id: &str) -> HubResult<Project> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id,name,slug,default_channel_id,archived_at_ms,version,created_at_ms,updated_at_ms
             FROM projects WHERE id=?1",
            [project_id],
            project_from_row,
        )
        .optional()?
        .ok_or_else(|| HubError::not_found("HUB_PROJECT_NOT_FOUND"))
    }

    pub fn list_members(&self, actor: &Actor, project_id: &str) -> HubResult<Vec<Membership>> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT m.project_id,m.principal_id,p.display_name,m.role,m.version
             FROM project_memberships m JOIN principals p ON p.id=m.principal_id
             WHERE m.project_id=?1 ORDER BY p.display_name,p.id",
        )?;
        let rows = statement.query_map([project_id], |row| {
            let role: String = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                role,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (project_id, principal_id, display_name, role, version) = row?;
            result.push(Membership {
                project_id,
                principal_id,
                display_name,
                role: Role::parse(&role)?,
                version,
            });
        }
        Ok(result)
    }

    pub fn upsert_member(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &UpsertMemberRequest,
    ) -> HubResult<Membership> {
        self.authorize_project(actor, project_id, Permission::ManageMembers)?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ManageMembers)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, "members.upsert", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let member_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_memberships WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_memberships WHERE project_id=?1 AND principal_id=?2)",
            params![project_id, request.principal_id],
            |row| row.get(0),
        )?;
        if !exists && member_count >= self.quotas.max_members {
            return Err(quota_error(
                "members",
                member_count,
                self.quotas.max_members,
            ));
        }
        let display_name = tx
            .query_row(
                "SELECT display_name FROM principals WHERE id=?1",
                [&request.principal_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_PRINCIPAL_NOT_FOUND"))?;
        let now = now_ms();
        tx.execute(
            "INSERT INTO project_memberships(project_id,principal_id,role,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?4)
             ON CONFLICT(project_id,principal_id) DO UPDATE SET role=excluded.role,
             version=project_memberships.version+1,updated_at_ms=excluded.updated_at_ms",
            params![project_id, request.principal_id, request.role.as_str(), now],
        )?;
        let membership = tx.query_row(
            "SELECT version FROM project_memberships WHERE project_id=?1 AND principal_id=?2",
            params![project_id, request.principal_id],
            |row| {
                Ok(Membership {
                    project_id: project_id.into(),
                    principal_id: request.principal_id.clone(),
                    display_name: display_name.clone(),
                    role: request.role,
                    version: row.get(0)?,
                })
            },
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "membership.upserted",
            "membership",
            &request.principal_id,
            "membership",
            "membership.changed",
            &serde_json::to_value(&membership).map_err(HubError::internal)?,
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "members.upsert",
            key,
            &request_hash,
            &membership,
        )?;
        tx.commit()?;
        Ok(membership)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpsertMemberRequest {
    pub principal_id: String,
    pub role: Role,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchProjectRequest {
    pub name: String,
    pub expected_version: i64,
}

impl SqliteRepository {
    pub fn patch_project(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &PatchProjectRequest,
    ) -> HubResult<Project> {
        self.authorize_project(actor, project_id, Permission::ManageMembers)?;
        validate_name(&request.name, 1, 120, "Project name")?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ManageMembers)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, "projects.patch", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let changed = tx.execute(
            "UPDATE projects SET name=?1,version=version+1,updated_at_ms=?2
             WHERE id=?3 AND version=?4 AND archived_at_ms IS NULL",
            params![
                request.name.trim(),
                now_ms(),
                project_id,
                request.expected_version
            ],
        )?;
        if changed != 1 {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Project version did not match.",
            ));
        }
        let project = tx.query_row(
            "SELECT id,name,slug,default_channel_id,archived_at_ms,version,created_at_ms,updated_at_ms FROM projects WHERE id=?1",
            [project_id],
            project_from_row,
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "project.updated",
            "project",
            project_id,
            "project",
            "project.updated",
            &json!({"name": project.name, "version": project.version}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "projects.patch",
            key,
            &request_hash,
            &project,
        )?;
        tx.commit()?;
        Ok(project)
    }

    pub fn archive_project(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
    ) -> HubResult<Project> {
        self.authorize_project(actor, project_id, Permission::ManageMembers)?;
        let request = json!({"projectId": project_id});
        let request_hash = canonical_request_hash(&request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ManageMembers)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "projects.archive",
            key,
            &request_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let now = now_ms();
        let changed = tx.execute(
            "UPDATE projects SET archived_at_ms=?1,version=version+1,updated_at_ms=?1 WHERE id=?2 AND archived_at_ms IS NULL",
            params![now, project_id],
        )?;
        if changed != 1 {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Project is already archived.",
            ));
        }
        let project = tx.query_row(
            "SELECT id,name,slug,default_channel_id,archived_at_ms,version,created_at_ms,updated_at_ms FROM projects WHERE id=?1",
            [project_id],
            project_from_row,
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "project.archived",
            "project",
            project_id,
            "project",
            "project.archived",
            &json!({"archivedAtMs": now}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "projects.archive",
            key,
            &request_hash,
            &project,
        )?;
        tx.commit()?;
        Ok(project)
    }

    pub fn remove_member(
        &self,
        actor: &Actor,
        project_id: &str,
        principal_id: &str,
        key: &str,
    ) -> HubResult<Value> {
        self.authorize_project(actor, project_id, Permission::ManageMembers)?;
        let request = json!({"principalId": principal_id});
        let request_hash = canonical_request_hash(&request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ManageMembers)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, "members.remove", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let role = tx
            .query_row(
                "SELECT role FROM project_memberships WHERE project_id=?1 AND principal_id=?2",
                params![project_id, principal_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_MEMBERSHIP_NOT_FOUND"))?;
        if role == "admin" {
            let admins: i64 = tx.query_row(
                "SELECT COUNT(*) FROM project_memberships WHERE project_id=?1 AND role='admin'",
                [project_id],
                |row| row.get(0),
            )?;
            if admins <= 1 {
                return Err(HubError::new(
                    StatusCode::CONFLICT,
                    "HUB_LAST_ADMIN",
                    "A project must retain at least one administrator.",
                ));
            }
        }
        tx.execute(
            "DELETE FROM project_memberships WHERE project_id=?1 AND principal_id=?2",
            params![project_id, principal_id],
        )?;
        let result = json!({"projectId": project_id, "principalId": principal_id, "removed": true});
        append_evidence(
            &tx,
            actor,
            project_id,
            "membership.removed",
            "membership",
            principal_id,
            "membership",
            "membership.removed",
            &result,
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "members.remove",
            key,
            &request_hash,
            &result,
        )?;
        tx.commit()?;
        Ok(result)
    }
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        slug: row.get(2)?,
        default_channel_id: row.get(3)?,
        archived_at_ms: row.get(4)?,
        version: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn validate_name(value: &str, min: usize, max: usize, name: &str) -> HubResult<()> {
    let trimmed = value.trim();
    if !(min..=max).contains(&trimmed.chars().count()) || trimmed.chars().any(char::is_control) {
        return Err(HubError::invalid(format!(
            "{name} must contain {min}-{max} non-control characters."
        )));
    }
    Ok(())
}

fn validate_slug(value: &str) -> HubResult<()> {
    if !(3..=64).contains(&value.len())
        || value.starts_with('-')
        || value.ends_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(HubError::invalid(
            "Project slug must be 3-64 lowercase letters, digits, or interior hyphens.",
        ));
    }
    Ok(())
}

fn quota_error(resource: &str, current: i64, limit: i64) -> HubError {
    HubError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "HUB_QUOTA_EXCEEDED",
        "The project quota would be exceeded.",
    )
    .details(json!({
        "resource": resource,
        "current": current,
        "limit": limit,
        "remediation": "Remove unused resources or ask the Hub operator to raise the quota."
    }))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtProposal {
    pub id: String,
    pub project_id: String,
    pub channel_id: String,
    pub base_revision_id: Option<String>,
    pub state_hash: String,
    pub title: String,
    pub description: String,
    pub resource_ids: Vec<String>,
    pub object_hashes: Vec<String>,
    pub status: String,
    pub created_by: String,
    pub accepted_revision_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtRevision {
    pub id: String,
    pub project_id: String,
    pub channel_id: String,
    pub parent_revision_id: Option<String>,
    pub proposal_id: String,
    pub state_hash: String,
    pub state: Value,
    pub created_by: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateArtProposalRequest {
    pub channel_id: String,
    pub base_revision_id: Option<String>,
    pub state_hash: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub resource_ids: Vec<String>,
    #[serde(default)]
    pub object_hashes: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewProposalRequest {
    pub verdict: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtReview {
    pub id: String,
    pub proposal_id: String,
    pub principal_id: String,
    pub verdict: String,
    pub body: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptProposalRequest {
    pub expected_head_revision_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RejectProposalRequest {
    pub reason: String,
}

impl SqliteRepository {
    pub fn create_art_proposal(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &CreateArtProposalRequest,
    ) -> HubResult<ArtProposal> {
        self.authorize_project(actor, project_id, Permission::ProposeArt)?;
        validate_name(&request.title, 1, 160, "Proposal title")?;
        validate_hash_label(&request.state_hash, "stateHash")?;
        let resources = sorted_unique(&request.resource_ids, "resourceIds")?;
        let object_hashes = sorted_unique(&request.object_hashes, "objectHashes")?;
        for hash in &object_hashes {
            validate_content_hash(hash)?;
        }
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ProposeArt)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.create",
            key,
            &request_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let channel = tx
            .query_row(
                "SELECT head_revision_id FROM art_channels WHERE id=?1 AND project_id=?2",
                params![request.channel_id, project_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_ART_CHANNEL_NOT_FOUND"))?;
        if channel != request.base_revision_id {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_BASE_STALE",
                "The proposal base is not the current accepted channel head.",
            )
            .details(json!({"currentHeadRevisionId": channel})));
        }
        for hash in &object_hashes {
            let authorized: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM project_objects WHERE project_id=?1 AND content_hash=?2)",
                params![project_id, hash],
                |row| row.get(0),
            )?;
            if !authorized {
                return Err(HubError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "HUB_OBJECT_MISSING",
                    "Every proposal object must be uploaded and authorized for the project.",
                ));
            }
        }
        let normalized_request = CreateArtProposalRequest {
            channel_id: request.channel_id.clone(),
            base_revision_id: request.base_revision_id.clone(),
            state_hash: request.state_hash.clone(),
            title: request.title.clone(),
            description: request.description.clone(),
            resource_ids: resources.clone(),
            object_hashes: object_hashes.clone(),
        };
        validate_canonical_art_proposal(
            &tx,
            &self.object_dir,
            project_id,
            &normalized_request,
            &channel,
        )?;
        let now = now_ms();
        let proposal = ArtProposal {
            id: new_id("apr"),
            project_id: project_id.into(),
            channel_id: request.channel_id.clone(),
            base_revision_id: request.base_revision_id.clone(),
            state_hash: request.state_hash.clone(),
            title: request.title.trim().into(),
            description: request.description.clone(),
            resource_ids: resources,
            object_hashes,
            status: "open".into(),
            created_by: actor.principal_id.clone(),
            accepted_revision_id: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        tx.execute(
            "INSERT INTO art_proposals(id,project_id,channel_id,base_revision_id,state_hash,title,
             description,resources_json,object_hashes_json,status,created_by,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'open',?10,?11,?11)",
            params![
                proposal.id,
                project_id,
                proposal.channel_id,
                proposal.base_revision_id,
                proposal.state_hash,
                proposal.title,
                proposal.description,
                serde_json::to_string(&proposal.resource_ids).map_err(HubError::internal)?,
                serde_json::to_string(&proposal.object_hashes).map_err(HubError::internal)?,
                actor.principal_id,
                now
            ],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "art_proposal.created",
            "art_proposal",
            &proposal.id,
            "art",
            "art.proposal.created",
            &json!({"proposalId": proposal.id, "channelId": proposal.channel_id}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.create",
            key,
            &request_hash,
            &proposal,
        )?;
        tx.commit()?;
        Ok(proposal)
    }

    pub fn get_art_proposal(
        &self,
        actor: &Actor,
        project_id: &str,
        proposal_id: &str,
    ) -> HubResult<ArtProposal> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id,project_id,channel_id,base_revision_id,state_hash,title,description,
             resources_json,object_hashes_json,status,created_by,accepted_revision_id,created_at_ms,
             updated_at_ms FROM art_proposals WHERE id=?1 AND project_id=?2",
            params![proposal_id, project_id],
            proposal_from_row,
        )
        .optional()?
        .ok_or_else(|| HubError::not_found("HUB_ART_PROPOSAL_NOT_FOUND"))
    }

    pub fn review_art_proposal(
        &self,
        actor: &Actor,
        project_id: &str,
        proposal_id: &str,
        key: &str,
        request: &ReviewProposalRequest,
    ) -> HubResult<ArtReview> {
        self.authorize_project(actor, project_id, Permission::AcceptArt)?;
        if !matches!(
            request.verdict.as_str(),
            "approve" | "request_changes" | "comment"
        ) {
            return Err(HubError::invalid("Unknown proposal review verdict."));
        }
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::AcceptArt)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.review",
            key,
            &request_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let status = tx
            .query_row(
                "SELECT status FROM art_proposals WHERE id=?1 AND project_id=?2",
                params![proposal_id, project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_ART_PROPOSAL_NOT_FOUND"))?;
        if status != "open" {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Only open proposals can be reviewed.",
            ));
        }
        let review = ArtReview {
            id: new_id("arv"),
            proposal_id: proposal_id.into(),
            principal_id: actor.principal_id.clone(),
            verdict: request.verdict.clone(),
            body: request.body.clone(),
            created_at_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO art_reviews(id,proposal_id,principal_id,verdict,body,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                review.id,
                review.proposal_id,
                review.principal_id,
                review.verdict,
                review.body,
                review.created_at_ms
            ],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "art_proposal.reviewed",
            "art_proposal",
            proposal_id,
            "art",
            "art.proposal.reviewed",
            &json!({"proposalId": proposal_id, "verdict": review.verdict}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.review",
            key,
            &request_hash,
            &review,
        )?;
        tx.commit()?;
        Ok(review)
    }

    pub fn accept_art_proposal(
        &self,
        actor: &Actor,
        project_id: &str,
        proposal_id: &str,
        key: &str,
        request: &AcceptProposalRequest,
    ) -> HubResult<ArtRevision> {
        self.authorize_project(actor, project_id, Permission::AcceptArt)?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::AcceptArt)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.accept",
            key,
            &request_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let proposal = tx
            .query_row(
                "SELECT id,project_id,channel_id,base_revision_id,state_hash,title,description,
                 resources_json,object_hashes_json,status,created_by,accepted_revision_id,created_at_ms,
                 updated_at_ms FROM art_proposals WHERE id=?1 AND project_id=?2",
                params![proposal_id, project_id],
                proposal_from_row,
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_ART_PROPOSAL_NOT_FOUND"))?;
        if proposal.status != "open" {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Only an open proposal can be accepted.",
            ));
        }
        let current_head = tx.query_row(
            "SELECT head_revision_id FROM art_channels WHERE id=?1 AND project_id=?2",
            params![proposal.channel_id, project_id],
            |row| row.get::<_, Option<String>>(0),
        )?;
        if current_head != request.expected_head_revision_id
            || current_head != proposal.base_revision_id
        {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_BASE_STALE",
                "The accepted channel head changed; rebase or merge the proposal.",
            )
            .details(json!({"currentHeadRevisionId": current_head})));
        }
        for hash in &proposal.object_hashes {
            let authorized: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM project_objects po JOIN objects o ON o.content_hash=po.content_hash
                 WHERE po.project_id=?1 AND po.content_hash=?2 AND o.integrity_status='verified')",
                params![project_id, hash],
                |row| row.get(0),
            )?;
            if !authorized {
                return Err(HubError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "HUB_OBJECT_MISSING",
                    "An accepted revision cannot reference missing or unverified objects.",
                ));
            }
        }
        let manifest = validate_canonical_art_proposal(
            &tx,
            &self.object_dir,
            project_id,
            &CreateArtProposalRequest {
                channel_id: proposal.channel_id.clone(),
                base_revision_id: proposal.base_revision_id.clone(),
                state_hash: proposal.state_hash.clone(),
                title: proposal.title.clone(),
                description: proposal.description.clone(),
                resource_ids: proposal.resource_ids.clone(),
                object_hashes: proposal.object_hashes.clone(),
            },
            &current_head,
        )?;
        let now = now_ms();
        for resource in &proposal.resource_ids {
            let covered: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM leases WHERE project_id=?1 AND channel_id=?2
                 AND resource_id=?3 AND holder_principal_id=?4 AND released_at_ms IS NULL
                 AND expires_at_ms>?5 AND base_revision_id IS ?6)",
                params![
                    project_id,
                    proposal.channel_id,
                    resource,
                    proposal.created_by,
                    now,
                    proposal.base_revision_id
                ],
                |row| row.get(0),
            )?;
            if !covered {
                return Err(HubError::new(
                    StatusCode::CONFLICT,
                    "HUB_LOCK_CONFLICT",
                    "Every changed protected resource requires an unexpired proposal-author lease.",
                )
                .details(json!({"resourceId": resource})));
            }
        }
        let approved: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM art_reviews WHERE proposal_id=?1 AND verdict='approve')",
            [proposal_id],
            |row| row.get(0),
        )?;
        // Admin acceptance is the approval in single-user development; otherwise
        // an explicit review is required to preserve separation of duties.
        let acceptor_role = Self::membership_role(&tx, actor, project_id, Permission::AcceptArt)?;
        if !approved && acceptor_role != Role::Admin {
            return Err(HubError::new(
                StatusCode::PRECONDITION_FAILED,
                "HUB_APPROVAL_REQUIRED",
                "This proposal needs an approving review before acceptance.",
            ));
        }
        let revision = ArtRevision {
            id: new_id("arev"),
            project_id: project_id.into(),
            channel_id: proposal.channel_id.clone(),
            parent_revision_id: current_head.clone(),
            proposal_id: proposal_id.into(),
            state_hash: proposal.state_hash.clone(),
            state: serde_json::to_value(&manifest).map_err(HubError::internal)?,
            created_by: actor.principal_id.clone(),
            created_at_ms: now,
        };
        tx.execute(
            "INSERT INTO art_revisions(id,project_id,channel_id,parent_revision_id,proposal_id,
             state_hash,state_json,created_by,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                revision.id,
                project_id,
                revision.channel_id,
                revision.parent_revision_id,
                proposal_id,
                revision.state_hash,
                serde_json::to_string(&revision.state).map_err(HubError::internal)?,
                actor.principal_id,
                now
            ],
        )?;
        let advanced = tx.execute(
            "UPDATE art_channels SET head_revision_id=?1,version=version+1
             WHERE id=?2 AND project_id=?3 AND head_revision_id IS ?4",
            params![revision.id, proposal.channel_id, project_id, current_head],
        )?;
        if advanced != 1 {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Accepted-head compare-and-swap failed.",
            ));
        }
        tx.execute(
            "UPDATE art_proposals SET status='accepted',accepted_revision_id=?1,updated_at_ms=?2
             WHERE id=?3 AND status='open'",
            params![revision.id, now, proposal_id],
        )?;
        for resource in &proposal.resource_ids {
            tx.execute(
                "UPDATE leases SET released_at_ms=?1,release_reason='proposal accepted',outcome_reference=?2 WHERE project_id=?3 AND channel_id=?4 AND resource_id=?5 AND holder_principal_id=?6 AND released_at_ms IS NULL",
                params![
                    now,
                    revision.id,
                    project_id,
                    proposal.channel_id,
                    resource,
                    proposal.created_by
                ],
            )?;
        }
        append_evidence(
            &tx,
            actor,
            project_id,
            "art_proposal.accepted",
            "art_revision",
            &revision.id,
            "art",
            "art.channel.head_changed",
            &json!({
                "proposalId": proposal_id,
                "revisionId": revision.id,
                "previousHeadRevisionId": current_head,
                "stateHash": revision.state_hash
            }),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.accept",
            key,
            &request_hash,
            &revision,
        )?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn get_art_revision(
        &self,
        actor: &Actor,
        project_id: &str,
        revision_id: &str,
    ) -> HubResult<ArtRevision> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id,project_id,channel_id,parent_revision_id,proposal_id,state_hash,state_json,
             created_by,created_at_ms FROM art_revisions WHERE id=?1 AND project_id=?2",
            params![revision_id, project_id],
            revision_from_row,
        )
        .optional()?
        .ok_or_else(|| HubError::not_found("HUB_ART_REVISION_NOT_FOUND"))
    }

    pub fn reject_art_proposal(
        &self,
        actor: &Actor,
        project_id: &str,
        proposal_id: &str,
        key: &str,
        request: &RejectProposalRequest,
    ) -> HubResult<ArtProposal> {
        self.authorize_project(actor, project_id, Permission::AcceptArt)?;
        validate_name(&request.reason, 3, 512, "reason")?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::AcceptArt)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.reject",
            key,
            &request_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let changed = tx.execute(
            "UPDATE art_proposals SET status='rejected',updated_at_ms=?1 WHERE id=?2 AND project_id=?3 AND status='open'",
            params![now_ms(), proposal_id, project_id],
        )?;
        if changed != 1 {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Only an open proposal can be rejected.",
            ));
        }
        let proposal = tx.query_row(
            "SELECT id,project_id,channel_id,base_revision_id,state_hash,title,description,resources_json,
             object_hashes_json,status,created_by,accepted_revision_id,created_at_ms,updated_at_ms
             FROM art_proposals WHERE id=?1", [proposal_id], proposal_from_row
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "art_proposal.rejected",
            "art_proposal",
            proposal_id,
            "art",
            "art.proposal.rejected",
            &json!({"proposalId": proposal_id, "reason": request.reason}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "art_proposals.reject",
            key,
            &request_hash,
            &proposal,
        )?;
        tx.commit()?;
        Ok(proposal)
    }

    pub fn channel_head(
        &self,
        actor: &Actor,
        project_id: &str,
        channel_id: &str,
    ) -> HubResult<Value> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        let value = conn
            .query_row(
                "SELECT head_revision_id,version FROM art_channels WHERE id=?1 AND project_id=?2",
                params![channel_id, project_id],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_ART_CHANNEL_NOT_FOUND"))?;
        Ok(json!({"channelId": channel_id, "headRevisionId": value.0, "version": value.1}))
    }
}

fn proposal_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtProposal> {
    let resources: String = row.get(7)?;
    let objects: String = row.get(8)?;
    Ok(ArtProposal {
        id: row.get(0)?,
        project_id: row.get(1)?,
        channel_id: row.get(2)?,
        base_revision_id: row.get(3)?,
        state_hash: row.get(4)?,
        title: row.get(5)?,
        description: row.get(6)?,
        resource_ids: serde_json::from_str(&resources).unwrap_or_default(),
        object_hashes: serde_json::from_str(&objects).unwrap_or_default(),
        status: row.get(9)?,
        created_by: row.get(10)?,
        accepted_revision_id: row.get(11)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
    })
}

fn validate_canonical_art_proposal(
    tx: &Transaction<'_>,
    object_dir: &std::path::Path,
    project_id: &str,
    request: &CreateArtProposalRequest,
    current_head: &Option<String>,
) -> HubResult<CanonicalArtManifest> {
    let mut manifest_candidates = Vec::new();
    for hash in &request.object_hashes {
        let media_type: Option<String> = tx
            .query_row(
                "SELECT o.media_type FROM objects o JOIN project_objects po ON po.content_hash=o.content_hash WHERE po.project_id=?1 AND o.content_hash=?2 AND o.integrity_status='verified'",
                params![project_id, hash],
                |row| row.get(0),
            )
            .optional()?;
        if media_type.as_deref() == Some(ART_MANIFEST_MEDIA_TYPE) {
            manifest_candidates.push(hash.clone());
        }
    }
    if manifest_candidates.len() != 1 {
        return Err(HubError::invalid(
            "A proposal must reference exactly one canonical art manifest object.",
        ));
    }
    let manifest_hash = &manifest_candidates[0];
    let digest = manifest_hash
        .strip_prefix("b3-256:")
        .ok_or_else(|| HubError::invalid("Art manifest hash is invalid."))?;
    let path = object_dir
        .join("cas")
        .join(&digest[0..2])
        .join(&digest[2..4])
        .join(digest);
    let metadata = std::fs::metadata(&path).map_err(HubError::internal)?;
    if metadata.len() == 0 || metadata.len() > MAX_ART_MANIFEST_BYTES as u64 {
        return Err(HubError::invalid("Art manifest size is outside limits."));
    }
    let bytes = std::fs::read(&path).map_err(HubError::internal)?;
    if content_hash(&bytes) != *manifest_hash {
        return Err(HubError::new(
            StatusCode::PRECONDITION_FAILED,
            "STO_HASH_MISMATCH",
            "The canonical art manifest failed content verification.",
        ));
    }
    let manifest: CanonicalArtManifest = serde_json::from_slice(&bytes)
        .map_err(|_| HubError::invalid("Canonical art manifest JSON is invalid."))?;
    let canonical = serde_jcs::to_vec(&manifest).map_err(HubError::internal)?;
    if canonical != bytes {
        return Err(HubError::invalid(
            "Art manifest bytes must use canonical JSON encoding.",
        ));
    }
    if manifest.schema_version != ART_MANIFEST_SCHEMA
        || manifest.project_id != project_id
        || manifest.channel_id != request.channel_id
        || manifest.base_hub_revision_id != *current_head
        || manifest.cells.is_empty()
        || manifest.cells.len() > 128
        || !valid_scoped_id(&manifest.source_session_id, "ses_")
        || !valid_scoped_id(&manifest.local_revision_id, "art_")
    {
        return Err(HubError::invalid(
            "Canonical art manifest context is invalid.",
        ));
    }

    let base_manifest = current_head
        .as_ref()
        .map(|revision_id| {
            tx.query_row(
                "SELECT state_json FROM art_revisions WHERE id=?1 AND project_id=?2 AND channel_id=?3",
                params![revision_id, project_id, request.channel_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(HubError::from)
            .and_then(|value| {
                serde_json::from_str::<CanonicalArtManifest>(&value).map_err(|_| {
                    HubError::new(
                        StatusCode::PRECONDITION_FAILED,
                        "HUB_ART_STATE_INVALID",
                        "The accepted base does not contain canonical art state.",
                    )
                })
            })
        })
        .transpose()?;
    let expected_base_root = base_manifest.as_ref().map(|base| base.state_root.clone());
    if manifest.base_state_root != expected_base_root {
        return Err(HubError::new(
            StatusCode::CONFLICT,
            "HUB_BASE_STALE",
            "The manifest base state root does not match the accepted channel head.",
        ));
    }

    let mut state = std::collections::BTreeMap::new();
    let mut cells_by_id = std::collections::BTreeMap::new();
    let mut expected_objects = BTreeSet::from([manifest_hash.clone()]);
    let mut previous_id: Option<&str> = None;
    for cell in &manifest.cells {
        if !valid_scoped_id(&cell.cell_id, "cell_")
            || cell.parent_path.len() > 1024
            || !cell.parent_path.starts_with('/')
            || cell.parent_path.chars().any(char::is_control)
            || cell.size_bytes == 0
            || previous_id.is_some_and(|previous| previous >= cell.cell_id.as_str())
        {
            return Err(HubError::invalid(
                "Canonical art cells must be sorted, unique, and contain valid slots.",
            ));
        }
        previous_id = Some(&cell.cell_id);
        validate_content_hash(&cell.content_hash)?;
        let object_size: Option<i64> = tx
            .query_row(
                "SELECT o.size_bytes FROM objects o JOIN project_objects po ON po.content_hash=o.content_hash WHERE po.project_id=?1 AND o.content_hash=?2 AND o.integrity_status='verified'",
                params![project_id, cell.content_hash],
                |row| row.get(0),
            )
            .optional()?;
        if object_size.and_then(|size| u64::try_from(size).ok()) != Some(cell.size_bytes) {
            return Err(HubError::new(
                StatusCode::PRECONDITION_FAILED,
                "HUB_OBJECT_MISSING",
                "A canonical art cell is missing, unauthorized, or has the wrong size.",
            ));
        }
        let cell_id = cell
            .cell_id
            .parse::<CellId>()
            .map_err(|_| HubError::invalid("Canonical art cell ID is invalid."))?;
        let snapshot_hash = cell
            .content_hash
            .parse::<ContentHash>()
            .map_err(|_| HubError::invalid("Canonical art cell hash is invalid."))?;
        state.insert(
            cell_id,
            DomainArtCellState {
                cell_id,
                snapshot_hash,
                slot_path: cell.parent_path.clone(),
            },
        );
        cells_by_id.insert(cell.cell_id.clone(), cell.clone());
        expected_objects.insert(cell.content_hash.clone());
    }
    let computed = DomainArtRevision::compute_state_root(&state)
        .map_err(|_| HubError::invalid("Canonical art state root could not be computed."))?
        .to_string();
    if manifest.state_root != computed || request.state_hash != computed {
        return Err(HubError::new(
            StatusCode::BAD_REQUEST,
            "HUB_ART_STATE_HASH_MISMATCH",
            "The proposal state root does not match its canonical cells.",
        ));
    }

    let mut derived_changed = BTreeSet::new();
    if let Some(base) = &base_manifest {
        let base_cells: std::collections::BTreeMap<_, _> = base
            .cells
            .iter()
            .map(|cell| (cell.cell_id.as_str(), cell))
            .collect();
        if base_cells
            .keys()
            .any(|cell_id| !cells_by_id.contains_key(*cell_id))
        {
            return Err(HubError::invalid(
                "Cell deletion is not supported by the v1 art manifest.",
            ));
        }
        for (cell_id, cell) in &cells_by_id {
            if base_cells
                .get(cell_id.as_str())
                .is_none_or(|base| *base != cell)
            {
                derived_changed.insert(cell_id.clone());
            }
        }
    } else {
        derived_changed.extend(cells_by_id.keys().cloned());
    }
    if derived_changed.is_empty()
        || manifest.changed_cell_ids != derived_changed.iter().cloned().collect::<Vec<_>>()
        || request.resource_ids != derived_changed.iter().cloned().collect::<Vec<_>>()
        || request.object_hashes != expected_objects.into_iter().collect::<Vec<_>>()
    {
        return Err(HubError::invalid(
            "Proposal resources and objects must exactly match the server-derived canonical art delta.",
        ));
    }
    Ok(manifest)
}

fn valid_scoped_id(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len() + 4
        && value.len() <= 256
        && value.starts_with(prefix)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn revision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtRevision> {
    let state: String = row.get(6)?;
    Ok(ArtRevision {
        id: row.get(0)?,
        project_id: row.get(1)?,
        channel_id: row.get(2)?,
        parent_revision_id: row.get(3)?,
        proposal_id: row.get(4)?,
        state_hash: row.get(5)?,
        state: serde_json::from_str(&state).unwrap_or(Value::Null),
        created_by: row.get(7)?,
        created_at_ms: row.get(8)?,
    })
}

fn sorted_unique(values: &[String], name: &str) -> HubResult<Vec<String>> {
    if values.len() > 10_000 {
        return Err(HubError::invalid(format!(
            "{name} contains too many entries."
        )));
    }
    let mut set = BTreeSet::new();
    for value in values {
        validate_name(value, 1, 256, name)?;
        if !set.insert(value.clone()) {
            return Err(HubError::invalid(format!(
                "{name} must not contain duplicates."
            )));
        }
    }
    Ok(set.into_iter().collect())
}

fn validate_hash_label(value: &str, name: &str) -> HubResult<()> {
    if !(16..=256).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte))
    {
        return Err(HubError::invalid(format!(
            "{name} is not a valid hash label."
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub id: String,
    pub project_id: String,
    pub channel_id: String,
    pub resource_id: String,
    pub base_revision_id: Option<String>,
    pub workstream: String,
    pub intended_action: String,
    pub cell_hash: Option<String>,
    pub holder_principal_id: String,
    pub holder_session_id: String,
    pub renewal_counter: i64,
    pub acquired_at_ms: i64,
    pub expires_at_ms: i64,
    pub released_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquireLeaseRequest {
    pub channel_id: String,
    pub resource_id: String,
    pub base_revision_id: Option<String>,
    pub workstream: String,
    pub intended_action: String,
    pub cell_hash: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquireLeaseBatchRequest {
    pub channel_id: String,
    pub resource_ids: Vec<String>,
    pub base_revision_id: Option<String>,
    pub workstream: String,
    pub intended_action: String,
    pub cell_hashes: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewLeaseRequest {
    pub renewal_counter: i64,
    pub base_revision_id: Option<String>,
    pub cell_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBatch {
    pub leases: Vec<Lease>,
    pub lease_duration_ms: i64,
    pub renewal_target_ms: i64,
    pub server_time_ms: i64,
}

impl SqliteRepository {
    pub fn acquire_lease(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &AcquireLeaseRequest,
    ) -> HubResult<LeaseBatch> {
        let batch = AcquireLeaseBatchRequest {
            channel_id: request.channel_id.clone(),
            resource_ids: vec![request.resource_id.clone()],
            base_revision_id: request.base_revision_id.clone(),
            workstream: request.workstream.clone(),
            intended_action: request.intended_action.clone(),
            cell_hashes: request
                .cell_hash
                .clone()
                .map(|hash| HashMap::from([(request.resource_id.clone(), hash)]))
                .unwrap_or_default(),
        };
        self.acquire_lease_batch(actor, project_id, key, &batch, "leases.acquire")
    }

    pub fn acquire_lease_batch(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &AcquireLeaseBatchRequest,
        route: &str,
    ) -> HubResult<LeaseBatch> {
        self.authorize_project(actor, project_id, Permission::ProposeArt)?;
        validate_name(&request.workstream, 1, 128, "workstream")?;
        validate_name(&request.intended_action, 1, 128, "intendedAction")?;
        let resources = sorted_unique(&request.resource_ids, "resourceIds")?;
        if resources.is_empty() {
            return Err(HubError::invalid(
                "At least one lease resource is required.",
            ));
        }
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ProposeArt)?;
        if let Some(value) = check_idempotency(&tx, actor, project_id, route, key, &request_hash)? {
            tx.commit()?;
            return Ok(value);
        }
        let head = tx
            .query_row(
                "SELECT head_revision_id FROM art_channels WHERE id=?1 AND project_id=?2",
                params![request.channel_id, project_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_ART_CHANNEL_NOT_FOUND"))?;
        if head != request.base_revision_id {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_BASE_STALE",
                "Lease base must be the current accepted channel head.",
            )
            .details(json!({"currentHeadRevisionId": head})));
        }
        let now = now_ms();
        let active_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM leases WHERE project_id=?1 AND released_at_ms IS NULL AND expires_at_ms>?2",
            params![project_id, now],
            |row| row.get(0),
        )?;
        if active_count + i64::try_from(resources.len()).unwrap_or(i64::MAX)
            > self.quotas.max_active_leases
        {
            return Err(quota_error(
                "activeLeases",
                active_count,
                self.quotas.max_active_leases,
            ));
        }
        for resource in &resources {
            let conflicting = tx
                .query_row(
                    "SELECT id,holder_principal_id,expires_at_ms FROM leases WHERE project_id=?1
                     AND channel_id=?2 AND resource_id=?3 AND released_at_ms IS NULL AND expires_at_ms>?4
                     ORDER BY expires_at_ms DESC LIMIT 1",
                    params![project_id, request.channel_id, resource, now],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((lock_id, holder, expires_at)) = conflicting {
                return Err(HubError::new(
                    StatusCode::CONFLICT,
                    "HUB_LOCK_CONFLICT",
                    "A protected acceptance lease already covers this resource.",
                )
                .details(json!({
                    "resourceId": resource,
                    "leaseId": lock_id,
                    "holderPrincipalId": holder,
                    "expiresAtMs": expires_at
                })));
            }
        }
        let mut leases = Vec::with_capacity(resources.len());
        for resource in resources {
            let lease = Lease {
                id: new_id("lck"),
                project_id: project_id.into(),
                channel_id: request.channel_id.clone(),
                resource_id: resource.clone(),
                base_revision_id: request.base_revision_id.clone(),
                workstream: request.workstream.clone(),
                intended_action: request.intended_action.clone(),
                cell_hash: request.cell_hashes.get(&resource).cloned(),
                holder_principal_id: actor.principal_id.clone(),
                holder_session_id: actor.session_id.clone(),
                renewal_counter: 0,
                acquired_at_ms: now,
                expires_at_ms: now + LEASE_DURATION_MS,
                released_at_ms: None,
            };
            tx.execute(
                "INSERT INTO leases(id,project_id,channel_id,resource_id,base_revision_id,workstream,
                 intended_action,cell_hash,holder_principal_id,holder_session_id,acquired_at_ms,expires_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    lease.id,
                    project_id,
                    lease.channel_id,
                    lease.resource_id,
                    lease.base_revision_id,
                    lease.workstream,
                    lease.intended_action,
                    lease.cell_hash,
                    actor.principal_id,
                    actor.session_id,
                    lease.acquired_at_ms,
                    lease.expires_at_ms
                ],
            )?;
            leases.push(lease);
        }
        let result = LeaseBatch {
            leases,
            lease_duration_ms: LEASE_DURATION_MS,
            renewal_target_ms: LEASE_RENEWAL_TARGET_MS,
            server_time_ms: now,
        };
        append_evidence(
            &tx,
            actor,
            project_id,
            "lease.batch_acquired",
            "lease_batch",
            &result.leases[0].id,
            "lock",
            "lock.acquired",
            &json!({
                "leaseIds": result.leases.iter().map(|lease| &lease.id).collect::<Vec<_>>(),
                "resourceIds": result.leases.iter().map(|lease| &lease.resource_id).collect::<Vec<_>>(),
                "expiresAtMs": now + LEASE_DURATION_MS
            }),
        )?;
        store_idempotency(&tx, actor, project_id, route, key, &request_hash, &result)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn list_leases(&self, actor: &Actor, project_id: &str) -> HubResult<Vec<Lease>> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let now = now_ms();
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id,project_id,channel_id,resource_id,base_revision_id,workstream,intended_action,
             cell_hash,holder_principal_id,holder_session_id,renewal_counter,acquired_at_ms,expires_at_ms,
             released_at_ms FROM leases WHERE project_id=?1 AND released_at_ms IS NULL
             AND expires_at_ms>?2 ORDER BY resource_id,id",
        )?;
        let rows = statement.query_map(params![project_id, now], lease_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn renew_lease(
        &self,
        actor: &Actor,
        project_id: &str,
        lease_id: &str,
        key: &str,
        request: &RenewLeaseRequest,
    ) -> HubResult<Lease> {
        self.authorize_project(actor, project_id, Permission::ProposeArt)?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ProposeArt)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, "leases.renew", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let now = now_ms();
        let changed = tx.execute(
            "UPDATE leases SET renewal_counter=renewal_counter+1,expires_at_ms=?1,
             base_revision_id=?2,cell_hash=?3 WHERE id=?4 AND project_id=?5
             AND holder_principal_id=?6 AND holder_session_id=?7 AND released_at_ms IS NULL
             AND expires_at_ms>?8 AND renewal_counter=?9",
            params![
                now + LEASE_DURATION_MS,
                request.base_revision_id,
                request.cell_hash,
                lease_id,
                project_id,
                actor.principal_id,
                actor.session_id,
                now,
                request.renewal_counter
            ],
        )?;
        if changed != 1 {
            let expiry = tx
                .query_row(
                    "SELECT expires_at_ms FROM leases WHERE id=?1 AND project_id=?2",
                    params![lease_id, project_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if expiry.is_some_and(|value| value <= now) {
                return Err(HubError::new(
                    StatusCode::CONFLICT,
                    "HUB_LOCK_EXPIRED",
                    "Expired leases cannot be renewed; acquire a new lease and rebase.",
                ));
            }
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Lease holder, session, or renewal counter did not match.",
            ));
        }
        let lease = tx.query_row(
            "SELECT id,project_id,channel_id,resource_id,base_revision_id,workstream,intended_action,
             cell_hash,holder_principal_id,holder_session_id,renewal_counter,acquired_at_ms,expires_at_ms,
             released_at_ms FROM leases WHERE id=?1",
            [lease_id],
            lease_from_row,
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "lease.renewed",
            "lease",
            lease_id,
            "lock",
            "lock.renewed",
            &json!({"leaseId": lease_id, "expiresAtMs": lease.expires_at_ms, "renewalCounter": lease.renewal_counter}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "leases.renew",
            key,
            &request_hash,
            &lease,
        )?;
        tx.commit()?;
        Ok(lease)
    }

    pub fn release_lease(
        &self,
        actor: &Actor,
        project_id: &str,
        lease_id: &str,
        reason: &str,
        force: bool,
    ) -> HubResult<Value> {
        let role = self.authorize_project(actor, project_id, Permission::ProposeArt)?;
        if force && role != Role::Admin {
            return Err(HubError::forbidden());
        }
        validate_name(reason, 3, 512, "release reason")?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        let sql = if force {
            "UPDATE leases SET released_at_ms=?1,release_reason=?2 WHERE id=?3 AND project_id=?4 AND released_at_ms IS NULL"
        } else {
            "UPDATE leases SET released_at_ms=?1,release_reason=?2 WHERE id=?3 AND project_id=?4
             AND holder_principal_id=?5 AND holder_session_id=?6 AND released_at_ms IS NULL"
        };
        let changed = if force {
            tx.execute(sql, params![now, reason, lease_id, project_id])?
        } else {
            tx.execute(
                sql,
                params![
                    now,
                    reason,
                    lease_id,
                    project_id,
                    actor.principal_id,
                    actor.session_id
                ],
            )?
        };
        if changed != 1 {
            return Err(HubError::not_found("HUB_LOCK_NOT_FOUND"));
        }
        let action = if force {
            "lease.broken"
        } else {
            "lease.released"
        };
        append_evidence(
            &tx,
            actor,
            project_id,
            action,
            "lease",
            lease_id,
            "lock",
            action,
            &json!({"leaseId": lease_id, "reason": reason, "forced": force}),
        )?;
        tx.commit()?;
        Ok(json!({"leaseId": lease_id, "releasedAtMs": now}))
    }
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        id: row.get(0)?,
        project_id: row.get(1)?,
        channel_id: row.get(2)?,
        resource_id: row.get(3)?,
        base_revision_id: row.get(4)?,
        workstream: row.get(5)?,
        intended_action: row.get(6)?,
        cell_hash: row.get(7)?,
        holder_principal_id: row.get(8)?,
        holder_session_id: row.get(9)?,
        renewal_counter: row.get(10)?,
        acquired_at_ms: row.get(11)?,
        expires_at_ms: row.get(12)?,
        released_at_ms: row.get(13)?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMetadata {
    pub content_hash: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub media_type: String,
    pub integrity_status: String,
    pub created_at_ms: i64,
    pub verified_at_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NegotiateUploadRequest {
    pub expected_hash: String,
    pub expected_size: i64,
    pub media_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadNegotiation {
    pub status: String,
    pub upload_id: Option<String>,
    pub upload_url: Option<String>,
    pub transfer_token: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub object: Option<ObjectMetadata>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteUploadRequest {
    pub purpose: String,
    pub referenced_by: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchStatRequest {
    pub content_hashes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectStatus {
    pub content_hash: String,
    pub status: String,
    pub size_bytes: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadNegotiation {
    pub download_id: String,
    pub download_url: String,
    pub transfer_token: String,
    pub expires_at_ms: i64,
    pub content_hash: String,
    pub size_bytes: i64,
}

impl SqliteRepository {
    pub fn negotiate_upload(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &NegotiateUploadRequest,
        public_base_url: &str,
    ) -> HubResult<UploadNegotiation> {
        self.authorize_project(actor, project_id, Permission::ProposeArt)?;
        validate_content_hash(&request.expected_hash)?;
        if request.expected_size < 0
            || usize::try_from(request.expected_size)
                .ok()
                .is_none_or(|size| size > self.quotas.max_upload_bytes)
        {
            return Err(quota_error(
                "uploadBytes",
                request.expected_size.max(0),
                i64::try_from(self.quotas.max_upload_bytes).unwrap_or(i64::MAX),
            ));
        }
        validate_media_type(&request.media_type)?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ProposeArt)?;
        if let Some(mut value) = check_idempotency::<UploadNegotiation>(
            &tx,
            actor,
            project_id,
            "objects.negotiate_upload",
            key,
            &request_hash,
        )? {
            if value.status == "upload_required"
                && value.transfer_token.is_none()
                && let (Some(upload_id), Some(expires_at)) =
                    (value.upload_id.as_deref(), value.expires_at_ms)
            {
                value.transfer_token = Some(derive_transfer_secret(
                    &self.transfer_key,
                    upload_id,
                    project_id,
                    expires_at,
                ));
            }
            tx.commit()?;
            return Ok(value);
        }
        let authorized = tx
            .query_row(
                "SELECT o.content_hash,o.sha256,o.size_bytes,o.media_type,o.integrity_status,
                 o.created_at_ms,o.verified_at_ms FROM objects o JOIN project_objects po
                 ON po.content_hash=o.content_hash WHERE po.project_id=?1 AND o.content_hash=?2",
                params![project_id, request.expected_hash],
                object_from_row,
            )
            .optional()?;
        if let Some(object) = authorized {
            if object.size_bytes != request.expected_size {
                return Err(HubError::new(
                    StatusCode::CONFLICT,
                    "STO_HASH_MISMATCH",
                    "The existing authorized object has a different size.",
                ));
            }
            let result = UploadNegotiation {
                status: "present".into(),
                upload_id: None,
                upload_url: None,
                transfer_token: None,
                expires_at_ms: None,
                object: Some(object),
            };
            store_idempotency(
                &tx,
                actor,
                project_id,
                "objects.negotiate_upload",
                key,
                &request_hash,
                &result,
            )?;
            tx.commit()?;
            return Ok(result);
        }
        let globally_present: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE content_hash=?1)",
            [&request.expected_hash],
            |row| row.get(0),
        )?;
        if globally_present {
            // Hash knowledge is deliberately insufficient to create a project
            // authorization reference.
            return Err(HubError::new(
                StatusCode::FORBIDDEN,
                "HUB_OBJECT_UNAUTHORIZED",
                "The object exists but is not authorized for this project.",
            ));
        }
        let used_bytes: i64 = tx.query_row(
            "SELECT COALESCE(SUM(o.size_bytes),0) FROM project_objects po
             JOIN objects o ON o.content_hash=po.content_hash WHERE po.project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if used_bytes.saturating_add(request.expected_size) > self.quotas.max_object_bytes {
            return Err(quota_error(
                "objectBytes",
                used_bytes,
                self.quotas.max_object_bytes,
            ));
        }
        let expires_at = now_ms() + TRANSFER_TTL_MS;
        let upload_id = new_id("upl");
        let secret = derive_transfer_secret(&self.transfer_key, &upload_id, project_id, expires_at);
        let temp_path = self.object_dir.join("tmp").join(&upload_id);
        tx.execute(
            "INSERT INTO uploads(id,project_id,expected_hash,expected_size,media_type,token_hash,
             temp_path,status,expires_at_ms,created_by,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'negotiated',?8,?9,?10)",
            params![
                upload_id,
                project_id,
                request.expected_hash,
                request.expected_size,
                request.media_type,
                token_hash(&secret),
                temp_path.to_string_lossy(),
                expires_at,
                actor.principal_id,
                now_ms()
            ],
        )?;
        let result = UploadNegotiation {
            status: "upload_required".into(),
            upload_id: Some(upload_id.clone()),
            upload_url: Some(format!(
                "{}/api/v1/transfers/uploads/{upload_id}",
                public_base_url.trim_end_matches('/')
            )),
            transfer_token: Some(secret),
            expires_at_ms: Some(expires_at),
            object: None,
        };
        append_evidence(
            &tx,
            actor,
            project_id,
            "object.upload_negotiated",
            "upload",
            &upload_id,
            "object",
            "object.upload.negotiated",
            &json!({"uploadId": upload_id, "contentHash": request.expected_hash, "sizeBytes": request.expected_size}),
        )?;
        let mut stored_result = result.clone();
        stored_result.transfer_token = None;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "objects.negotiate_upload",
            key,
            &request_hash,
            &stored_result,
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn receive_upload(
        &self,
        upload_id: &str,
        transfer_token: &str,
        bytes: &Bytes,
    ) -> HubResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let upload = tx
            .query_row(
                "SELECT expected_hash,expected_size,token_hash,temp_path,status,expires_at_ms
                 FROM uploads WHERE id=?1",
                [upload_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("STO_UPLOAD_EXPIRED"))?;
        if upload.4 != "negotiated" || upload.5 <= now_ms() {
            return Err(HubError::new(
                StatusCode::GONE,
                "STO_UPLOAD_EXPIRED",
                "The upload negotiation is no longer active.",
            ));
        }
        if token_hash(transfer_token)
            .as_bytes()
            .ct_eq(upload.2.as_bytes())
            .unwrap_u8()
            != 1
        {
            return Err(HubError::unauthorized());
        }
        if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != upload.1 {
            return Err(HubError::new(
                StatusCode::BAD_REQUEST,
                "STO_HASH_MISMATCH",
                "Uploaded byte length does not match the negotiated size.",
            ));
        }
        let actual_hash = content_hash(bytes);
        if actual_hash != upload.0 {
            return Err(HubError::new(
                StatusCode::BAD_REQUEST,
                "STO_HASH_MISMATCH",
                "Uploaded bytes do not match the negotiated BLAKE3 content hash.",
            ));
        }
        let path = PathBuf::from(upload.3);
        let stage_path = path.with_extension("stage");
        std::fs::write(&stage_path, bytes).map_err(HubError::internal)?;
        std::fs::rename(&stage_path, &path).map_err(HubError::internal)?;
        tx.execute(
            "UPDATE uploads SET status='received',actual_sha256=?1 WHERE id=?2 AND status='negotiated'",
            params![sha256_hex(bytes), upload_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn complete_upload(
        &self,
        actor: &Actor,
        project_id: &str,
        upload_id: &str,
        key: &str,
        request: &CompleteUploadRequest,
    ) -> HubResult<ObjectMetadata> {
        self.authorize_project(actor, project_id, Permission::ProposeArt)?;
        validate_name(&request.purpose, 1, 128, "purpose")?;
        validate_name(&request.referenced_by, 1, 256, "referencedBy")?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ProposeArt)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "uploads.complete",
            key,
            &request_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let upload = tx
            .query_row(
                "SELECT expected_hash,expected_size,media_type,temp_path,status,actual_sha256,expires_at_ms
                 FROM uploads WHERE id=?1 AND project_id=?2",
                params![upload_id, project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("STO_UPLOAD_EXPIRED"))?;
        if upload.4 != "received" || upload.6 <= now_ms() {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "STO_UPLOAD_EXPIRED",
                "Upload bytes have not been received or the transfer expired.",
            ));
        }
        let temp_path = PathBuf::from(&upload.3);
        let bytes = std::fs::read(&temp_path).map_err(HubError::internal)?;
        if content_hash(&bytes) != upload.0
            || i64::try_from(bytes.len()).unwrap_or(i64::MAX) != upload.1
            || upload.5.as_deref() != Some(sha256_hex(&bytes).as_str())
        {
            return Err(HubError::new(
                StatusCode::BAD_REQUEST,
                "STO_HASH_MISMATCH",
                "Upload verification failed at completion.",
            ));
        }
        let object_path = self.object_path(&upload.0)?;
        if let Some(parent) = object_path.parent() {
            std::fs::create_dir_all(parent).map_err(HubError::internal)?;
        }
        if object_path.exists() {
            let existing = std::fs::read(&object_path).map_err(HubError::internal)?;
            if content_hash(&existing) != upload.0 {
                return Err(HubError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "STO_OBJECT_CORRUPT",
                    "A physical object failed integrity verification.",
                ));
            }
            let _ = std::fs::remove_file(&temp_path);
        } else {
            std::fs::rename(&temp_path, &object_path).map_err(HubError::internal)?;
        }
        let now = now_ms();
        let object = ObjectMetadata {
            content_hash: upload.0,
            sha256: upload.5.unwrap_or_else(|| sha256_hex(&bytes)),
            size_bytes: upload.1,
            media_type: upload.2,
            integrity_status: "verified".into(),
            created_at_ms: now,
            verified_at_ms: now,
        };
        tx.execute(
            "INSERT OR IGNORE INTO objects(content_hash,sha256,size_bytes,media_type,storage_path,
             integrity_status,created_at_ms,verified_at_ms) VALUES(?1,?2,?3,?4,?5,'verified',?6,?6)",
            params![
                object.content_hash,
                object.sha256,
                object.size_bytes,
                object.media_type,
                object_path.to_string_lossy(),
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO project_objects(project_id,content_hash,purpose,referenced_by,created_at_ms)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                project_id,
                object.content_hash,
                request.purpose,
                request.referenced_by,
                now
            ],
        )?;
        tx.execute(
            "UPDATE uploads SET status='completed' WHERE id=?1 AND status='received'",
            [upload_id],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "object.upload_completed",
            "object",
            &object.content_hash,
            "object",
            "object.available",
            &json!({"contentHash": object.content_hash, "sizeBytes": object.size_bytes, "uploadId": upload_id}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "uploads.complete",
            key,
            &request_hash,
            &object,
        )?;
        tx.commit()?;
        Ok(object)
    }

    pub fn object_metadata(
        &self,
        actor: &Actor,
        project_id: &str,
        content_hash: &str,
    ) -> HubResult<ObjectMetadata> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        validate_content_hash(content_hash)?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT o.content_hash,o.sha256,o.size_bytes,o.media_type,o.integrity_status,
             o.created_at_ms,o.verified_at_ms FROM objects o JOIN project_objects po
             ON po.content_hash=o.content_hash WHERE po.project_id=?1 AND o.content_hash=?2",
            params![project_id, content_hash],
            object_from_row,
        )
        .optional()?
        .ok_or_else(|| HubError::not_found("STO_OBJECT_UNAUTHORIZED"))
    }

    pub fn batch_stat(
        &self,
        actor: &Actor,
        project_id: &str,
        request: &BatchStatRequest,
    ) -> HubResult<Vec<ObjectStatus>> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let hashes = sorted_unique(&request.content_hashes, "contentHashes")?;
        if hashes.len() > 500 {
            return Err(HubError::invalid("batchStat accepts at most 500 hashes."));
        }
        let conn = self.conn()?;
        let mut result = Vec::with_capacity(hashes.len());
        for hash in hashes {
            validate_content_hash(&hash)?;
            let authorized = conn
                .query_row(
                    "SELECT o.size_bytes FROM objects o JOIN project_objects po
                     ON po.content_hash=o.content_hash WHERE po.project_id=?1 AND o.content_hash=?2",
                    params![project_id, hash],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let globally_present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE content_hash=?1)",
                [&hash],
                |row| row.get(0),
            )?;
            result.push(ObjectStatus {
                content_hash: hash,
                status: if authorized.is_some() {
                    "present_authorized"
                } else if globally_present {
                    "present_unauthorized"
                } else {
                    "missing"
                }
                .into(),
                size_bytes: authorized,
            });
        }
        Ok(result)
    }

    pub fn negotiate_download(
        &self,
        actor: &Actor,
        project_id: &str,
        content_hash: &str,
        public_base_url: &str,
    ) -> HubResult<DownloadNegotiation> {
        let object = self.object_metadata(actor, project_id, content_hash)?;
        let id = new_id("dwn");
        let secret = random_secret();
        let expires_at = now_ms() + TRANSFER_TTL_MS;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO download_transfers(id,project_id,content_hash,token_hash,expires_at_ms,
             created_by,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                project_id,
                content_hash,
                token_hash(&secret),
                expires_at,
                actor.principal_id,
                now_ms()
            ],
        )?;
        Ok(DownloadNegotiation {
            download_id: id.clone(),
            download_url: format!(
                "{}/api/v1/transfers/downloads/{id}",
                public_base_url.trim_end_matches('/')
            ),
            transfer_token: secret,
            expires_at_ms: expires_at,
            content_hash: content_hash.into(),
            size_bytes: object.size_bytes,
        })
    }

    pub fn read_download(
        &self,
        download_id: &str,
        transfer_token: &str,
    ) -> HubResult<(Vec<u8>, String)> {
        let conn = self.conn()?;
        let transfer = conn
            .query_row(
                "SELECT d.token_hash,d.expires_at_ms,o.storage_path,o.content_hash,o.media_type
                 FROM download_transfers d JOIN objects o ON o.content_hash=d.content_hash
                 JOIN project_objects po ON po.project_id=d.project_id AND po.content_hash=d.content_hash
                 WHERE d.id=?1",
                [download_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("STO_OBJECT_UNAUTHORIZED"))?;
        if transfer.1 <= now_ms() {
            return Err(HubError::new(
                StatusCode::GONE,
                "STO_UPLOAD_EXPIRED",
                "The download transfer expired.",
            ));
        }
        if token_hash(transfer_token)
            .as_bytes()
            .ct_eq(transfer.0.as_bytes())
            .unwrap_u8()
            != 1
        {
            return Err(HubError::unauthorized());
        }
        let bytes = std::fs::read(&transfer.2).map_err(HubError::internal)?;
        if content_hash(&bytes) != transfer.3 {
            return Err(HubError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "STO_OBJECT_CORRUPT",
                "Downloaded bytes failed content verification.",
            ));
        }
        Ok((bytes, transfer.4))
    }

    fn object_path(&self, content_hash: &str) -> HubResult<PathBuf> {
        validate_content_hash(content_hash)?;
        let digest = content_hash
            .strip_prefix("b3-256:")
            .ok_or_else(|| HubError::invalid("Unsupported content hash."))?;
        Ok(self
            .object_dir
            .join("cas")
            .join(&digest[0..2])
            .join(&digest[2..4])
            .join(digest))
    }
}

fn object_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObjectMetadata> {
    Ok(ObjectMetadata {
        content_hash: row.get(0)?,
        sha256: row.get(1)?,
        size_bytes: row.get(2)?,
        media_type: row.get(3)?,
        integrity_status: row.get(4)?,
        created_at_ms: row.get(5)?,
        verified_at_ms: row.get(6)?,
    })
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    use rand::RngCore;
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn derive_transfer_secret(
    transfer_key: &[u8; 32],
    upload_id: &str,
    project_id: &str,
    expires_at_ms: i64,
) -> String {
    let material =
        format!("neuman-hub-upload-transfer-v1\0{upload_id}\0{project_id}\0{expires_at_ms}");
    hex::encode(blake3::keyed_hash(transfer_key, material.as_bytes()).as_bytes())
}

fn content_hash(bytes: impl AsRef<[u8]>) -> String {
    let digest = blake3::hash(bytes.as_ref());
    format!("b3-256:{}", base32_lower(digest.as_bytes()))
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut output = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        accumulator = (accumulator << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(char::from(ALPHABET[((accumulator >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        output.push(char::from(
            ALPHABET[((accumulator << (5 - bits)) & 31) as usize],
        ));
    }
    output
}

fn validate_content_hash(value: &str) -> HubResult<()> {
    let Some(digest) = value.strip_prefix("b3-256:") else {
        return Err(HubError::new(
            StatusCode::BAD_REQUEST,
            "STO_HASH_INVALID",
            "Content hash must use b3-256 identity.",
        ));
    };
    if digest.len() != 52
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
    {
        return Err(HubError::new(
            StatusCode::BAD_REQUEST,
            "STO_HASH_INVALID",
            "BLAKE3 digest must be 52 lowercase base32 characters.",
        ));
    }
    Ok(())
}

fn validate_media_type(value: &str) -> HubResult<()> {
    if !(3..=128).contains(&value.len())
        || !value.contains('/')
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(HubError::invalid("mediaType is invalid."));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRecord {
    pub id: String,
    pub project_id: String,
    pub logical_hash: String,
    pub status: String,
    pub evidence: Value,
    pub created_by: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBuildRequest {
    pub logical_hash: String,
    #[serde(default)]
    pub evidence: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildAttempt {
    pub id: String,
    pub build_id: String,
    pub status: String,
    pub evidence: Value,
    pub created_by: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBuildAttemptRequest {
    pub status: String,
    #[serde(default)]
    pub evidence: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRecord {
    pub id: String,
    pub project_id: String,
    pub bundle_hash: String,
    pub environment: String,
    pub target: Value,
    pub evidence: Value,
    pub request_hash: String,
    pub state: String,
    pub created_by: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateReleaseRequest {
    pub bundle_hash: String,
    pub environment: String,
    pub target: Value,
    #[serde(default)]
    pub evidence: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseApproval {
    pub id: String,
    pub release_id: String,
    pub principal_id: String,
    pub request_hash: String,
    pub evidence: Value,
    pub created_at_ms: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveReleaseRequest {
    pub request_hash: String,
    #[serde(default)]
    pub evidence: Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseActionRequest {
    pub expected_request_hash: String,
    #[serde(default)]
    pub evidence: Value,
}

impl SqliteRepository {
    pub fn create_build(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &CreateBuildRequest,
    ) -> HubResult<BuildRecord> {
        self.authorize_project(actor, project_id, Permission::Build)?;
        validate_hash_label(&request.logical_hash, "logicalHash")?;
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::Build)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, "builds.create", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let now = now_ms();
        let build = BuildRecord {
            id: new_id("bld"),
            project_id: project_id.into(),
            logical_hash: request.logical_hash.clone(),
            status: "queued".into(),
            evidence: request.evidence.clone(),
            created_by: actor.principal_id.clone(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        tx.execute(
            "INSERT INTO builds(id,project_id,logical_hash,status,evidence_json,created_by,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
            params![
                build.id,
                project_id,
                build.logical_hash,
                build.status,
                serde_json::to_string(&build.evidence).map_err(HubError::internal)?,
                actor.principal_id,
                now
            ],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "build.created",
            "build",
            &build.id,
            "build",
            "build.created",
            &json!({"buildId": build.id, "logicalHash": build.logical_hash}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "builds.create",
            key,
            &request_hash,
            &build,
        )?;
        tx.commit()?;
        Ok(build)
    }

    pub fn get_build(
        &self,
        actor: &Actor,
        project_id: &str,
        build_id: &str,
    ) -> HubResult<BuildRecord> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id,project_id,logical_hash,status,evidence_json,created_by,created_at_ms,updated_at_ms
             FROM builds WHERE id=?1 AND project_id=?2",
            params![build_id, project_id],
            build_from_row,
        )
        .optional()?
        .ok_or_else(|| HubError::not_found("HUB_BUILD_NOT_FOUND"))
    }

    pub fn add_build_attempt(
        &self,
        actor: &Actor,
        project_id: &str,
        build_id: &str,
        key: &str,
        request: &CreateBuildAttemptRequest,
    ) -> HubResult<BuildAttempt> {
        self.authorize_project(actor, project_id, Permission::Build)?;
        if !matches!(
            request.status.as_str(),
            "running" | "succeeded" | "failed" | "cancelled"
        ) {
            return Err(HubError::invalid("Unknown build attempt status."));
        }
        let request_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::Build)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, "builds.attempt", key, &request_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM builds WHERE id=?1 AND project_id=?2)",
            params![build_id, project_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(HubError::not_found("HUB_BUILD_NOT_FOUND"));
        }
        let attempt = BuildAttempt {
            id: new_id("bat"),
            build_id: build_id.into(),
            status: request.status.clone(),
            evidence: request.evidence.clone(),
            created_by: actor.principal_id.clone(),
            created_at_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO build_attempts(id,build_id,status,evidence_json,created_by,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                attempt.id,
                build_id,
                attempt.status,
                serde_json::to_string(&attempt.evidence).map_err(HubError::internal)?,
                actor.principal_id,
                attempt.created_at_ms
            ],
        )?;
        tx.execute(
            "UPDATE builds SET status=?1,updated_at_ms=?2 WHERE id=?3 AND project_id=?4",
            params![attempt.status, attempt.created_at_ms, build_id, project_id],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "build.attempt_recorded",
            "build",
            build_id,
            "build",
            "build.status_changed",
            &json!({"buildId": build_id, "attemptId": attempt.id, "status": attempt.status}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "builds.attempt",
            key,
            &request_hash,
            &attempt,
        )?;
        tx.commit()?;
        Ok(attempt)
    }

    pub fn create_release(
        &self,
        actor: &Actor,
        project_id: &str,
        key: &str,
        request: &CreateReleaseRequest,
    ) -> HubResult<ReleaseRecord> {
        self.authorize_project(actor, project_id, Permission::ExecuteRelease)?;
        validate_hash_label(&request.bundle_hash, "bundleHash")?;
        validate_name(&request.environment, 1, 64, "environment")?;
        let immutable_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ExecuteRelease)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "releases.create",
            key,
            &immutable_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let now = now_ms();
        let release = ReleaseRecord {
            id: new_id("rel"),
            project_id: project_id.into(),
            bundle_hash: request.bundle_hash.clone(),
            environment: request.environment.clone(),
            target: request.target.clone(),
            evidence: request.evidence.clone(),
            request_hash: immutable_hash.clone(),
            state: "draft".into(),
            created_by: actor.principal_id.clone(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        tx.execute(
            "INSERT INTO releases(id,project_id,bundle_hash,environment,target_json,evidence_json,
             request_hash,state,created_by,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'draft',?8,?9,?9)",
            params![
                release.id,
                project_id,
                release.bundle_hash,
                release.environment,
                serde_json::to_string(&release.target).map_err(HubError::internal)?,
                serde_json::to_string(&release.evidence).map_err(HubError::internal)?,
                release.request_hash,
                actor.principal_id,
                now
            ],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "release.created",
            "release",
            &release.id,
            "release",
            "release.created",
            &json!({"releaseId": release.id, "requestHash": release.request_hash, "environment": release.environment}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "releases.create",
            key,
            &immutable_hash,
            &release,
        )?;
        tx.commit()?;
        Ok(release)
    }

    pub fn get_release(
        &self,
        actor: &Actor,
        project_id: &str,
        release_id: &str,
    ) -> HubResult<ReleaseRecord> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id,project_id,bundle_hash,environment,target_json,evidence_json,request_hash,
             state,created_by,created_at_ms,updated_at_ms FROM releases WHERE id=?1 AND project_id=?2",
            params![release_id, project_id],
            release_from_row,
        )
        .optional()?
        .ok_or_else(|| HubError::not_found("HUB_RELEASE_NOT_FOUND"))
    }

    pub fn approve_release(
        &self,
        actor: &Actor,
        project_id: &str,
        release_id: &str,
        key: &str,
        request: &ApproveReleaseRequest,
    ) -> HubResult<ReleaseApproval> {
        self.authorize_project(actor, project_id, Permission::ApproveRelease)?;
        let request_body_hash = canonical_request_hash(request)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ApproveRelease)?;
        if let Some(value) = check_idempotency(
            &tx,
            actor,
            project_id,
            "releases.approve",
            key,
            &request_body_hash,
        )? {
            tx.commit()?;
            return Ok(value);
        }
        let immutable_hash = tx
            .query_row(
                "SELECT request_hash FROM releases WHERE id=?1 AND project_id=?2 AND state IN ('draft','approved')",
                params![release_id, project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_RELEASE_NOT_FOUND"))?;
        if immutable_hash != request.request_hash {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Approval does not bind the release's immutable request hash.",
            ));
        }
        let approval = ReleaseApproval {
            id: new_id("app"),
            release_id: release_id.into(),
            principal_id: actor.principal_id.clone(),
            request_hash: immutable_hash,
            evidence: request.evidence.clone(),
            created_at_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO release_approvals(id,release_id,principal_id,request_hash,evidence_json,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                approval.id,
                release_id,
                actor.principal_id,
                approval.request_hash,
                serde_json::to_string(&approval.evidence).map_err(HubError::internal)?,
                approval.created_at_ms
            ],
        )?;
        tx.execute(
            "UPDATE releases SET state='approved',updated_at_ms=?1 WHERE id=?2 AND state='draft'",
            params![approval.created_at_ms, release_id],
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            "release.approved",
            "release",
            release_id,
            "release",
            "release.approved",
            &json!({"releaseId": release_id, "approvalId": approval.id, "requestHash": approval.request_hash}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            "releases.approve",
            key,
            &request_body_hash,
            &approval,
        )?;
        tx.commit()?;
        Ok(approval)
    }

    pub fn release_action(
        &self,
        actor: &Actor,
        project_id: &str,
        release_id: &str,
        action: &str,
        key: &str,
        request: &ReleaseActionRequest,
    ) -> HubResult<ReleaseRecord> {
        self.authorize_project(actor, project_id, Permission::ExecuteRelease)?;
        let (required_states, next_state) = match action {
            "start" => (&["approved"][..], "running"),
            "resume" => (&["failed", "rollback_required"][..], "running"),
            "rollback" => (
                &["running", "failed", "rollback_required", "succeeded"][..],
                "rolled_back",
            ),
            _ => return Err(HubError::invalid("Unknown release action.")),
        };
        let request_body_hash = canonical_request_hash(request)?;
        let route = format!("releases.{action}");
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::membership_role(&tx, actor, project_id, Permission::ExecuteRelease)?;
        if let Some(value) =
            check_idempotency(&tx, actor, project_id, &route, key, &request_body_hash)?
        {
            tx.commit()?;
            return Ok(value);
        }
        let release = tx
            .query_row(
                "SELECT id,project_id,bundle_hash,environment,target_json,evidence_json,request_hash,
                 state,created_by,created_at_ms,updated_at_ms FROM releases WHERE id=?1 AND project_id=?2",
                params![release_id, project_id],
                release_from_row,
            )
            .optional()?
            .ok_or_else(|| HubError::not_found("HUB_RELEASE_NOT_FOUND"))?;
        if release.request_hash != request.expected_request_hash
            || !required_states.contains(&release.state.as_str())
        {
            return Err(HubError::new(
                StatusCode::CONFLICT,
                "HUB_VERSION_CONFLICT",
                "Release state or immutable request hash does not permit this action.",
            ));
        }
        if action == "start" {
            let approval_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM release_approvals WHERE release_id=?1 AND request_hash=?2",
                params![release_id, release.request_hash],
                |row| row.get(0),
            )?;
            if approval_count < 1 {
                return Err(HubError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "HUB_APPROVAL_REQUIRED",
                    "Release start requires approval bound to the immutable request hash.",
                ));
            }
        }
        let now = now_ms();
        tx.execute(
            "UPDATE releases SET state=?1,updated_at_ms=?2,evidence_json=?3 WHERE id=?4",
            params![
                next_state,
                now,
                serde_json::to_string(&request.evidence).map_err(HubError::internal)?,
                release_id
            ],
        )?;
        let updated = tx.query_row(
            "SELECT id,project_id,bundle_hash,environment,target_json,evidence_json,request_hash,
             state,created_by,created_at_ms,updated_at_ms FROM releases WHERE id=?1",
            [release_id],
            release_from_row,
        )?;
        append_evidence(
            &tx,
            actor,
            project_id,
            &format!("release.{action}"),
            "release",
            release_id,
            "release",
            &format!("release.{action}"),
            &json!({"releaseId": release_id, "state": next_state, "requestHash": release.request_hash}),
        )?;
        store_idempotency(
            &tx,
            actor,
            project_id,
            &route,
            key,
            &request_body_hash,
            &updated,
        )?;
        tx.commit()?;
        Ok(updated)
    }
}

fn build_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BuildRecord> {
    let evidence: String = row.get(4)?;
    Ok(BuildRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        logical_hash: row.get(2)?,
        status: row.get(3)?,
        evidence: serde_json::from_str(&evidence).unwrap_or(Value::Null),
        created_by: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn release_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReleaseRecord> {
    let target: String = row.get(4)?;
    let evidence: String = row.get(5)?;
    Ok(ReleaseRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        bundle_hash: row.get(2)?,
        environment: row.get(3)?,
        target: serde_json::from_str(&target).unwrap_or(Value::Null),
        evidence: serde_json::from_str(&evidence).unwrap_or(Value::Null),
        request_hash: row.get(6)?,
        state: row.get(7)?,
        created_by: row.get(8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub sequence: i64,
    pub id: String,
    pub project_id: String,
    pub actor_principal_id: String,
    pub action: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub outcome: String,
    pub details: Value,
    pub occurred_at_ms: i64,
    pub previous_hash: Option<String>,
    pub event_hash: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageQuery {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPage {
    pub events: Vec<EventEnvelope>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditPage {
    pub events: Vec<AuditEvent>,
    pub next_cursor: Option<String>,
}

impl SqliteRepository {
    pub fn events_page(
        &self,
        actor: &Actor,
        project_id: &str,
        after: i64,
        limit: usize,
        config: &HubConfig,
    ) -> HubResult<EventPage> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        let minimum: Option<i64> = conn.query_row(
            "SELECT MIN(sequence) FROM outbox_events WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if after > 0 && minimum.is_some_and(|minimum| after < minimum - 1) {
            return Err(HubError::new(
                StatusCode::GONE,
                "HUB_CURSOR_EXPIRED",
                "The event cursor is outside retained history; fetch current state.",
            ));
        }
        let mut statement = conn.prepare(
            "SELECT id,sequence,project_id,category,event_type,aggregate_id,payload_json,occurred_at_ms
             FROM outbox_events WHERE project_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![project_id, after, i64::try_from(limit).unwrap_or(200)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )?;
        let mut events = Vec::new();
        for row in rows {
            let row = row?;
            events.push(EventEnvelope {
                id: row.0,
                sequence: row.1,
                cursor: sign_cursor(row.1, config),
                project_id: row.2,
                category: row.3,
                event_type: row.4,
                aggregate_id: row.5,
                payload: serde_json::from_str(&row.6).unwrap_or(Value::Null),
                occurred_at_ms: row.7,
            });
        }
        let next_cursor = events.last().map(|event| event.cursor.clone());
        Ok(EventPage {
            events,
            next_cursor,
        })
    }

    pub fn audit_page(
        &self,
        actor: &Actor,
        project_id: &str,
        after: i64,
        limit: usize,
        config: &HubConfig,
    ) -> HubResult<AuditPage> {
        self.authorize_project(actor, project_id, Permission::Read)?;
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT sequence,id,project_id,actor_principal_id,action,aggregate_type,aggregate_id,
             outcome,details_json,occurred_at_ms,previous_hash,event_hash FROM audit_events
             WHERE project_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![project_id, after, i64::try_from(limit).unwrap_or(200)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                ))
            },
        )?;
        let mut events = Vec::new();
        for row in rows {
            let row = row?;
            events.push(AuditEvent {
                sequence: row.0,
                id: row.1,
                project_id: row.2,
                actor_principal_id: row.3,
                action: row.4,
                aggregate_type: row.5,
                aggregate_id: row.6,
                outcome: row.7,
                details: serde_json::from_str(&row.8).unwrap_or(Value::Null),
                occurred_at_ms: row.9,
                previous_hash: row.10,
                event_hash: row.11,
            });
        }
        let next_cursor = events
            .last()
            .map(|event| sign_cursor(event.sequence, config));
        Ok(AuditPage {
            events,
            next_cursor,
        })
    }

    pub fn latest_event(
        &self,
        project_id: &str,
        config: &HubConfig,
    ) -> HubResult<Option<EventEnvelope>> {
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT id,sequence,project_id,category,event_type,aggregate_id,payload_json,occurred_at_ms
                 FROM outbox_events WHERE project_id=?1 ORDER BY sequence DESC LIMIT 1",
                [project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(|row| EventEnvelope {
            id: row.0,
            sequence: row.1,
            cursor: sign_cursor(row.1, config),
            project_id: row.2,
            category: row.3,
            event_type: row.4,
            aggregate_id: row.5,
            payload: serde_json::from_str(&row.6).unwrap_or(Value::Null),
            occurred_at_ms: row.7,
        }))
    }

    pub fn maintenance(&self, event_retention: i64) -> HubResult<Value> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = tx.prepare(
            "SELECT temp_path FROM uploads WHERE expires_at_ms<=?1 AND status IN ('negotiated','received')",
        )?;
        let temp_paths = statement
            .query_map([now], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let expired_uploads = tx.execute(
            "UPDATE uploads SET status='expired' WHERE expires_at_ms<=?1 AND status IN ('negotiated','received')",
            [now],
        )?;
        let expired_downloads = tx.execute(
            "DELETE FROM download_transfers WHERE expires_at_ms<=?1",
            [now],
        )?;
        let expired_idempotency = tx.execute(
            "DELETE FROM idempotency_records WHERE expires_at_ms<=?1",
            [now],
        )?;
        let retained = event_retention.max(1);
        let pruned_events = tx.execute(
            "DELETE FROM outbox_events WHERE sequence IN (
               SELECT sequence FROM (
                 SELECT sequence, ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY sequence DESC) AS retained_rank
                 FROM outbox_events
               ) WHERE retained_rank>?1
             )",
            [retained],
        )?;
        tx.commit()?;
        for path in temp_paths {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => warn!(error = %error, "could not remove expired upload temp object"),
            }
        }
        Ok(json!({
            "expiredUploads": expired_uploads,
            "expiredDownloads": expired_downloads,
            "expiredIdempotencyRecords": expired_idempotency,
            "prunedOutboxEvents": pruned_events
        }))
    }
}

fn sign_cursor(sequence: i64, config: &HubConfig) -> String {
    let payload = format!("v1.{sequence}");
    let mac = blake3::keyed_hash(&config.cursor_key, payload.as_bytes());
    format!("{payload}.{}", &hex::encode(mac.as_bytes())[..32])
}

fn parse_cursor(cursor: Option<&str>, config: &HubConfig) -> HubResult<i64> {
    let Some(cursor) = cursor else { return Ok(0) };
    let mut fields = cursor.split('.');
    let version = fields.next();
    let sequence = fields.next();
    let supplied_mac = fields.next();
    if fields.next().is_some() || version != Some("v1") {
        return Err(HubError::invalid("Event cursor is invalid."));
    }
    let sequence = sequence
        .ok_or_else(|| HubError::invalid("Event cursor is invalid."))?
        .parse::<i64>()
        .map_err(|_| HubError::invalid("Event cursor is invalid."))?;
    let expected = sign_cursor(sequence, config);
    let expected_mac = expected.rsplit('.').next().unwrap_or_default();
    let supplied_mac = supplied_mac.unwrap_or_default();
    if supplied_mac.len() != expected_mac.len()
        || supplied_mac
            .as_bytes()
            .ct_eq(expected_mac.as_bytes())
            .unwrap_u8()
            != 1
    {
        return Err(HubError::invalid("Event cursor signature is invalid."));
    }
    Ok(sequence)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Presence {
    pub project_id: String,
    pub principal_id: String,
    pub display_name: String,
    pub place_id: Option<String>,
    pub channel_id: Option<String>,
    pub mode: String,
    pub selected_cell_ids: Vec<String>,
    pub lock_ids: Vec<String>,
    pub last_heartbeat_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PresenceRequest {
    pub place_id: Option<String>,
    pub channel_id: Option<String>,
    pub mode: String,
    #[serde(default)]
    pub selected_cell_ids: Vec<String>,
    #[serde(default)]
    pub lock_ids: Vec<String>,
    #[serde(default)]
    pub hide_selection: bool,
}

/// Build the versioned HTTP API. The returned router owns all state and can be
/// embedded in tests or served with `axum::serve`.
pub fn build_router(state: AppState) -> Router {
    let max_body = state.config.quotas.max_upload_bytes;
    let request_id = HeaderName::from_static("x-request-id");
    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/api/v1/version", get(version))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/me", get(me))
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{project_id}",
            get(get_project).patch(patch_project).post(archive_project),
        )
        .route(
            "/api/v1/projects/{project_id}/members",
            get(list_members).post(upsert_member),
        )
        .route(
            "/api/v1/projects/{project_id}/members/{principal_id}",
            patch(update_member).delete(remove_member),
        )
        .route(
            "/api/v1/projects/{project_id}/art-channels/{channel_id}/head",
            get(channel_head),
        )
        .route(
            "/api/v1/projects/{project_id}/art-proposals",
            post(create_art_proposal),
        )
        .route(
            "/api/v1/projects/{project_id}/art-proposals/{proposal_id}",
            get(get_art_proposal).post(art_proposal_action),
        )
        .route(
            "/api/v1/projects/{project_id}/art-proposals/{proposal_id}/reviews",
            post(review_art_proposal),
        )
        .route(
            "/api/v1/projects/{project_id}/art-revisions/{revision_id}",
            get(get_art_revision),
        )
        .route(
            "/api/v1/projects/{project_id}/art-revisions/{revision_id}/state",
            get(get_art_revision_state),
        )
        .route("/api/v1/projects/{project_id}/locks", get(list_leases))
        .route(
            "/api/v1/projects/{project_id}/locks:acquire",
            post(acquire_lease),
        )
        .route(
            "/api/v1/projects/{project_id}/locks:acquireBatch",
            post(acquire_lease_batch),
        )
        .route(
            "/api/v1/projects/{project_id}/locks/{lease_id}",
            delete(release_lease).post(lease_action),
        )
        .route(
            "/api/v1/projects/{project_id}/objects:negotiateUpload",
            post(negotiate_upload),
        )
        .route(
            "/api/v1/projects/{project_id}/uploads/{upload_id}",
            post(complete_upload),
        )
        .route(
            "/api/v1/projects/{project_id}/objects:batchStat",
            post(batch_stat),
        )
        .route(
            "/api/v1/projects/{project_id}/objects/{content_hash}",
            get(object_get_action),
        )
        .route("/api/v1/transfers/uploads/{upload_id}", put(receive_upload))
        .route(
            "/api/v1/transfers/downloads/{download_id}",
            get(read_download),
        )
        .route("/api/v1/projects/{project_id}/builds", post(create_build))
        .route(
            "/api/v1/projects/{project_id}/builds/{build_id}",
            get(get_build).post(cancel_build),
        )
        .route(
            "/api/v1/projects/{project_id}/builds/{build_id}/attempts",
            post(add_build_attempt),
        )
        .route(
            "/api/v1/projects/{project_id}/releases",
            post(create_release),
        )
        .route(
            "/api/v1/projects/{project_id}/releases/{release_id}",
            get(get_release).post(release_action),
        )
        .route(
            "/api/v1/projects/{project_id}/releases/{release_id}/approvals",
            post(approve_release),
        )
        .route("/api/v1/projects/{project_id}/events", get(events_page))
        .route(
            "/api/v1/projects/{project_id}/audit-events",
            get(audit_page),
        )
        .route("/api/v1/events/stream", get(event_stream))
        .route(
            "/api/v1/projects/{project_id}/presence:heartbeat",
            post(presence_heartbeat),
        )
        .route("/api/v1/projects/{project_id}/presence", get(list_presence))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health_live() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "live"})))
}

async fn health_ready(State(state): State<AppState>) -> HubResult<Json<Value>> {
    state.repo.readiness()?;
    Ok(Json(json!({
        "status": "ready",
        "capabilities": {
            "metadataRead": true,
            "contentRead": true,
            "contentWrite": true,
            "locks": true,
            "events": true,
            "builds": true,
            "releases": true
        }
    })))
}

async fn version() -> Json<Value> {
    Json(json!({
        "apiVersion": API_VERSION,
        "implementation": "neuman-hub",
        "version": env!("CARGO_PKG_VERSION"),
        "storageProfile": "sqlite-local-cas"
    }))
}

async fn capabilities() -> Json<Value> {
    Json(json!({
        "api": ["projects", "memberships", "art-cas", "leases", "objects", "events", "build-evidence", "release-evidence", "audit"],
        "leaseDurationMs": LEASE_DURATION_MS,
        "leaseRenewalTargetMs": LEASE_RENEWAL_TARGET_MS,
        "presenceTtlMs": PRESENCE_TTL_MS,
        "eventDelivery": "at-least-once",
        "objectTransfer": "scoped-local-bearer",
        "productionAdapters": ["postgresql", "s3"]
    }))
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> HubResult<Json<Actor>> {
    Ok(Json(authenticate(&state.repo, &headers)?))
}

async fn list_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HubResult<Json<Vec<Project>>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.list_projects(&actor)?))
}

async fn create_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateProjectRequest>,
) -> HubResult<(StatusCode, Json<Project>)> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state
        .repo
        .create_project(&actor, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &result.id);
    Ok((StatusCode::CREATED, Json(result)))
}

async fn get_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> HubResult<Json<Project>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.get_project(&actor, &project_id)?))
}

async fn patch_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<PatchProjectRequest>,
) -> HubResult<Json<Project>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result =
        state
            .repo
            .patch_project(&actor, &project_id, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn archive_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> HubResult<Json<Project>> {
    let project_id = require_action(&project_id, "archive")?.to_owned();
    let actor = authenticate(&state.repo, &headers)?;
    let result = state
        .repo
        .archive_project(&actor, &project_id, &idempotency_key(&headers)?)?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn list_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> HubResult<Json<Vec<Membership>>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.list_members(&actor, &project_id)?))
}

async fn upsert_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<UpsertMemberRequest>,
) -> HubResult<(StatusCode, Json<Membership>)> {
    let actor = authenticate(&state.repo, &headers)?;
    let result =
        state
            .repo
            .upsert_member(&actor, &project_id, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &project_id);
    Ok((StatusCode::CREATED, Json(result)))
}

async fn update_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, principal_id)): Path<(String, String)>,
    Json(mut request): Json<UpsertMemberRequest>,
) -> HubResult<Json<Membership>> {
    if request.principal_id != principal_id {
        return Err(HubError::invalid("Path and body principalId must match."));
    }
    request.principal_id = principal_id;
    let actor = authenticate(&state.repo, &headers)?;
    let result =
        state
            .repo
            .upsert_member(&actor, &project_id, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn remove_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, principal_id)): Path<(String, String)>,
) -> HubResult<Json<Value>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.remove_member(
        &actor,
        &project_id,
        &principal_id,
        &idempotency_key(&headers)?,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn channel_head(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, channel_id)): Path<(String, String)>,
) -> HubResult<Json<Value>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.channel_head(
        &actor,
        &project_id,
        &channel_id,
    )?))
}

macro_rules! mutation_handler {
    ($name:ident, $request:ty, $response:ty, $method:ident, $path:pat, $args:expr) => {
        async fn $name(
            State(state): State<AppState>,
            headers: HeaderMap,
            Path($path): Path<(String, String)>,
            Json(request): Json<$request>,
        ) -> HubResult<Json<$response>> {
            let actor = authenticate(&state.repo, &headers)?;
            let key = idempotency_key(&headers)?;
            let (project_id, resource_id) = $args;
            let result = state
                .repo
                .$method(&actor, &project_id, &resource_id, &key, &request)?;
            publish_latest(&state, &project_id);
            Ok(Json(result))
        }
    };
}

async fn create_art_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<CreateArtProposalRequest>,
) -> HubResult<(StatusCode, Json<ArtProposal>)> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.create_art_proposal(
        &actor,
        &project_id,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok((StatusCode::CREATED, Json(result)))
}

async fn get_art_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, proposal_id)): Path<(String, String)>,
) -> HubResult<Json<ArtProposal>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.get_art_proposal(
        &actor,
        &project_id,
        &proposal_id,
    )?))
}

mutation_handler!(
    review_art_proposal,
    ReviewProposalRequest,
    ArtReview,
    review_art_proposal,
    (project_id, proposal_id),
    (project_id, proposal_id)
);
mutation_handler!(
    accept_art_proposal,
    AcceptProposalRequest,
    ArtRevision,
    accept_art_proposal,
    (project_id, proposal_id),
    (project_id, proposal_id)
);
mutation_handler!(
    reject_art_proposal,
    RejectProposalRequest,
    ArtProposal,
    reject_art_proposal,
    (project_id, proposal_id),
    (project_id, proposal_id)
);

async fn art_proposal_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, proposal_action)): Path<(String, String)>,
    Json(payload): Json<Value>,
) -> HubResult<Json<Value>> {
    if let Ok(proposal_id) = require_action(&proposal_action, "accept") {
        let request = serde_json::from_value::<AcceptProposalRequest>(payload)
            .map_err(|_| HubError::invalid("Invalid proposal acceptance request."))?;
        let result = accept_art_proposal(
            State(state),
            headers,
            Path((project_id, proposal_id.to_owned())),
            Json(request),
        )
        .await?;
        return Ok(Json(
            serde_json::to_value(result.0).map_err(HubError::internal)?,
        ));
    }
    if let Ok(proposal_id) = require_action(&proposal_action, "reject") {
        let request = serde_json::from_value::<RejectProposalRequest>(payload)
            .map_err(|_| HubError::invalid("Invalid proposal rejection request."))?;
        let result = reject_art_proposal(
            State(state),
            headers,
            Path((project_id, proposal_id.to_owned())),
            Json(request),
        )
        .await?;
        return Ok(Json(
            serde_json::to_value(result.0).map_err(HubError::internal)?,
        ));
    }
    Err(HubError::not_found("HUB_ART_PROPOSAL_ACTION_NOT_FOUND"))
}

async fn get_art_revision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, revision_id)): Path<(String, String)>,
) -> HubResult<Json<ArtRevision>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.get_art_revision(
        &actor,
        &project_id,
        &revision_id,
    )?))
}

async fn get_art_revision_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, revision_id)): Path<(String, String)>,
) -> HubResult<Json<Value>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(
        state
            .repo
            .get_art_revision(&actor, &project_id, &revision_id)?
            .state,
    ))
}

async fn list_leases(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> HubResult<Json<Vec<Lease>>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.list_leases(&actor, &project_id)?))
}

async fn acquire_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<AcquireLeaseRequest>,
) -> HubResult<Json<LeaseBatch>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result =
        state
            .repo
            .acquire_lease(&actor, &project_id, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn acquire_lease_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<AcquireLeaseBatchRequest>,
) -> HubResult<Json<LeaseBatch>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.acquire_lease_batch(
        &actor,
        &project_id,
        &idempotency_key(&headers)?,
        &request,
        "leases.acquire_batch",
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn renew_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, lease_id)): Path<(String, String)>,
    Json(request): Json<RenewLeaseRequest>,
) -> HubResult<Json<Lease>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.renew_lease(
        &actor,
        &project_id,
        &lease_id,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

#[derive(Deserialize, Default)]
struct ReasonQuery {
    reason: Option<String>,
}

async fn release_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, lease_id)): Path<(String, String)>,
    Query(query): Query<ReasonQuery>,
) -> HubResult<Json<Value>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.release_lease(
        &actor,
        &project_id,
        &lease_id,
        query.reason.as_deref().unwrap_or("work completed"),
        false,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn break_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, lease_id)): Path<(String, String)>,
    Json(request): Json<RejectProposalRequest>,
) -> HubResult<Json<Value>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state
        .repo
        .release_lease(&actor, &project_id, &lease_id, &request.reason, true)?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn lease_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, lease_action)): Path<(String, String)>,
    Json(payload): Json<Value>,
) -> HubResult<Json<Value>> {
    if let Ok(lease_id) = require_action(&lease_action, "renew") {
        let request = serde_json::from_value::<RenewLeaseRequest>(payload)
            .map_err(|_| HubError::invalid("Invalid lease renewal request."))?;
        let result = renew_lease(
            State(state),
            headers,
            Path((project_id, lease_id.to_owned())),
            Json(request),
        )
        .await?;
        return Ok(Json(
            serde_json::to_value(result.0).map_err(HubError::internal)?,
        ));
    }
    if let Ok(lease_id) = require_action(&lease_action, "break") {
        let request = serde_json::from_value::<RejectProposalRequest>(payload)
            .map_err(|_| HubError::invalid("Invalid lease break request."))?;
        let result = break_lease(
            State(state),
            headers,
            Path((project_id, lease_id.to_owned())),
            Json(request),
        )
        .await?;
        return Ok(Json(result.0));
    }
    Err(HubError::not_found("HUB_LOCK_ACTION_NOT_FOUND"))
}

async fn negotiate_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<NegotiateUploadRequest>,
) -> HubResult<Json<UploadNegotiation>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.negotiate_upload(
        &actor,
        &project_id,
        &idempotency_key(&headers)?,
        &request,
        &state.config.public_base_url,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

fn transfer_token(headers: &HeaderMap) -> HubResult<&str> {
    headers
        .get("x-neuman-transfer-token")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 256)
        .ok_or_else(HubError::unauthorized)
}

async fn receive_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(upload_id): Path<String>,
    bytes: Bytes,
) -> HubResult<StatusCode> {
    state
        .repo
        .receive_upload(&upload_id, transfer_token(&headers)?, &bytes)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn complete_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, upload_id)): Path<(String, String)>,
    Json(request): Json<CompleteUploadRequest>,
) -> HubResult<Json<ObjectMetadata>> {
    let upload_id = require_action(&upload_id, "complete")?.to_owned();
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.complete_upload(
        &actor,
        &project_id,
        &upload_id,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn batch_stat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<BatchStatRequest>,
) -> HubResult<Json<Vec<ObjectStatus>>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.batch_stat(
        &actor,
        &project_id,
        &request,
    )?))
}

async fn object_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, content_hash)): Path<(String, String)>,
) -> HubResult<Json<ObjectMetadata>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.object_metadata(
        &actor,
        &project_id,
        &content_hash,
    )?))
}

async fn negotiate_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, content_hash)): Path<(String, String)>,
) -> HubResult<Json<DownloadNegotiation>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.negotiate_download(
        &actor,
        &project_id,
        &content_hash,
        &state.config.public_base_url,
    )?))
}

async fn object_get_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, content_action)): Path<(String, String)>,
) -> HubResult<Json<Value>> {
    if let Ok(content_hash) = require_action(&content_action, "download") {
        let result = negotiate_download(
            State(state),
            headers,
            Path((project_id, content_hash.to_owned())),
        )
        .await?;
        return Ok(Json(
            serde_json::to_value(result.0).map_err(HubError::internal)?,
        ));
    }
    let result = object_metadata(State(state), headers, Path((project_id, content_action))).await?;
    Ok(Json(
        serde_json::to_value(result.0).map_err(HubError::internal)?,
    ))
}

async fn read_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(download_id): Path<String>,
) -> HubResult<Response> {
    let (bytes, media_type) = state
        .repo
        .read_download(&download_id, transfer_token(&headers)?)?;
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(response)
}

async fn create_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<CreateBuildRequest>,
) -> HubResult<(StatusCode, Json<BuildRecord>)> {
    let actor = authenticate(&state.repo, &headers)?;
    let result =
        state
            .repo
            .create_build(&actor, &project_id, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &project_id);
    Ok((StatusCode::CREATED, Json(result)))
}

async fn get_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, build_id)): Path<(String, String)>,
) -> HubResult<Json<BuildRecord>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.get_build(
        &actor,
        &project_id,
        &build_id,
    )?))
}

async fn add_build_attempt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, build_id)): Path<(String, String)>,
    Json(request): Json<CreateBuildAttemptRequest>,
) -> HubResult<Json<BuildAttempt>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.add_build_attempt(
        &actor,
        &project_id,
        &build_id,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn cancel_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, build_id)): Path<(String, String)>,
    Json(mut request): Json<CreateBuildAttemptRequest>,
) -> HubResult<Json<BuildAttempt>> {
    let build_id = require_action(&build_id, "cancel")?.to_owned();
    request.status = "cancelled".into();
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.add_build_attempt(
        &actor,
        &project_id,
        &build_id,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn create_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<CreateReleaseRequest>,
) -> HubResult<(StatusCode, Json<ReleaseRecord>)> {
    let actor = authenticate(&state.repo, &headers)?;
    let result =
        state
            .repo
            .create_release(&actor, &project_id, &idempotency_key(&headers)?, &request)?;
    publish_latest(&state, &project_id);
    Ok((StatusCode::CREATED, Json(result)))
}

async fn get_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, release_id)): Path<(String, String)>,
) -> HubResult<Json<ReleaseRecord>> {
    let actor = authenticate(&state.repo, &headers)?;
    Ok(Json(state.repo.get_release(
        &actor,
        &project_id,
        &release_id,
    )?))
}

async fn approve_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, release_id)): Path<(String, String)>,
    Json(request): Json<ApproveReleaseRequest>,
) -> HubResult<Json<ReleaseApproval>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.approve_release(
        &actor,
        &project_id,
        &release_id,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

async fn release_action_handler(
    state: AppState,
    headers: HeaderMap,
    project_id: String,
    release_id: String,
    action: &'static str,
    request: ReleaseActionRequest,
) -> HubResult<Json<ReleaseRecord>> {
    let actor = authenticate(&state.repo, &headers)?;
    let result = state.repo.release_action(
        &actor,
        &project_id,
        &release_id,
        action,
        &idempotency_key(&headers)?,
        &request,
    )?;
    publish_latest(&state, &project_id);
    Ok(Json(result))
}

macro_rules! release_action_handler {
    ($name:ident, $action:literal) => {
        async fn $name(
            State(state): State<AppState>,
            headers: HeaderMap,
            Path((project_id, release_id)): Path<(String, String)>,
            Json(request): Json<ReleaseActionRequest>,
        ) -> HubResult<Json<ReleaseRecord>> {
            release_action_handler(state, headers, project_id, release_id, $action, request).await
        }
    };
}
release_action_handler!(start_release, "start");
release_action_handler!(resume_release, "resume");
release_action_handler!(rollback_release, "rollback");

async fn release_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project_id, release_action)): Path<(String, String)>,
    Json(request): Json<ReleaseActionRequest>,
) -> HubResult<Json<ReleaseRecord>> {
    if let Ok(release_id) = require_action(&release_action, "start") {
        return start_release(
            State(state),
            headers,
            Path((project_id, release_id.to_owned())),
            Json(request),
        )
        .await;
    }
    if let Ok(release_id) = require_action(&release_action, "resume") {
        return resume_release(
            State(state),
            headers,
            Path((project_id, release_id.to_owned())),
            Json(request),
        )
        .await;
    }
    if let Ok(release_id) = require_action(&release_action, "rollback") {
        return rollback_release(
            State(state),
            headers,
            Path((project_id, release_id.to_owned())),
            Json(request),
        )
        .await;
    }
    Err(HubError::not_found("HUB_RELEASE_ACTION_NOT_FOUND"))
}

fn require_action<'a>(value: &'a str, action: &str) -> HubResult<&'a str> {
    value
        .strip_suffix(&format!(":{action}"))
        .filter(|identifier| !identifier.is_empty())
        .ok_or_else(|| HubError::not_found("HUB_ACTION_NOT_FOUND"))
}

fn page_limit(query: &PageQuery) -> usize {
    usize::from(query.limit.unwrap_or(50).clamp(1, 200))
}

async fn events_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> HubResult<Json<EventPage>> {
    let actor = authenticate(&state.repo, &headers)?;
    let after = parse_cursor(query.cursor.as_deref(), &state.config)?;
    Ok(Json(state.repo.events_page(
        &actor,
        &project_id,
        after,
        page_limit(&query),
        &state.config,
    )?))
}

async fn audit_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> HubResult<Json<AuditPage>> {
    let actor = authenticate(&state.repo, &headers)?;
    let after = parse_cursor(query.cursor.as_deref(), &state.config)?;
    Ok(Json(state.repo.audit_page(
        &actor,
        &project_id,
        after,
        page_limit(&query),
        &state.config,
    )?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamQuery {
    project_id: String,
    cursor: Option<String>,
}

async fn event_stream(
    websocket: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> HubResult<Response> {
    let actor = authenticate(&state.repo, &headers)?;
    state
        .repo
        .authorize_project(&actor, &query.project_id, Permission::Read)?;
    let after = parse_cursor(query.cursor.as_deref(), &state.config)?;
    let replay = state
        .repo
        .events_page(&actor, &query.project_id, after, 200, &state.config)?
        .events;
    let project_id = query.project_id;
    let receiver = state.events.subscribe();
    Ok(websocket.on_upgrade(move |socket| websocket_session(socket, project_id, replay, receiver)))
}

async fn websocket_session(
    mut socket: axum::extract::ws::WebSocket,
    project_id: String,
    replay: Vec<EventEnvelope>,
    mut receiver: broadcast::Receiver<EventEnvelope>,
) {
    for event in replay {
        let Ok(payload) = serde_json::to_string(&event) else {
            return;
        };
        if socket.send(Message::Text(payload.into())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(event) if event.project_id == project_id => {
                    let Ok(payload) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::Text(payload.into())).await.is_err() { break; }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

async fn presence_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(request): Json<PresenceRequest>,
) -> HubResult<Json<Presence>> {
    let actor = authenticate(&state.repo, &headers)?;
    state
        .repo
        .authorize_project(&actor, &project_id, Permission::Read)?;
    if !matches!(request.mode.as_str(), "editing" | "reviewing" | "building") {
        return Err(HubError::invalid("Unknown presence mode."));
    }
    let now = now_ms();
    let presence = Presence {
        project_id: project_id.clone(),
        principal_id: actor.principal_id.clone(),
        display_name: actor.display_name,
        place_id: request.place_id,
        channel_id: request.channel_id,
        mode: request.mode,
        selected_cell_ids: if request.hide_selection {
            Vec::new()
        } else {
            sorted_unique(&request.selected_cell_ids, "selectedCellIds")?
        },
        lock_ids: sorted_unique(&request.lock_ids, "lockIds")?,
        last_heartbeat_ms: now,
        expires_at_ms: now + PRESENCE_TTL_MS,
    };
    let key = format!("{}:{}:{}", project_id, actor.principal_id, actor.session_id);
    state
        .presence
        .lock()
        .map_err(|_| HubError::internal("presence lock poisoned"))?
        .insert(key, presence.clone());
    Ok(Json(presence))
}

async fn list_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> HubResult<Json<Vec<Presence>>> {
    let actor = authenticate(&state.repo, &headers)?;
    state
        .repo
        .authorize_project(&actor, &project_id, Permission::Read)?;
    let now = now_ms();
    let mut presence = state
        .presence
        .lock()
        .map_err(|_| HubError::internal("presence lock poisoned"))?;
    presence.retain(|_, item| item.expires_at_ms > now);
    let mut result = presence
        .values()
        .filter(|item| item.project_id == project_id)
        .cloned()
        .collect::<Vec<_>>();
    result.sort_by(|left, right| left.principal_id.cmp(&right.principal_id));
    Ok(Json(result))
}

fn publish_latest(state: &AppState, project_id: &str) {
    match state.repo.latest_event(project_id, &state.config) {
        Ok(Some(event)) => {
            let _ = state.events.send(event);
        }
        Ok(None) => {}
        Err(error) => warn!(
            project_id,
            error_code = error.code,
            "could not publish committed outbox event"
        ),
    }
}

/// Start the Hub from environment configuration and shut down gracefully on
/// Ctrl-C (and SIGTERM on Unix).
pub async fn run_from_env() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .try_init()
        .ok();
    let config = Arc::new(HubConfig::from_env()?);
    let repo = Arc::new(SqliteRepository::open(&config)?);
    if let Some(token) = &config.bootstrap_token {
        let principal = repo.bootstrap_development_principal(token, &config.bootstrap_name)?;
        info!(
            principal_id = principal,
            "development bootstrap principal is ready"
        );
    }
    repo.readiness()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    repo.maintenance(config.event_retention)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let maintenance_repo = Arc::clone(&repo);
    let event_retention = config.event_retention;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = maintenance_repo.maintenance(event_retention) {
                warn!(error_code = error.code, "Hub background maintenance failed");
            }
        }
    });
    let (events, _) = broadcast::channel(2048);
    let state = AppState {
        repo,
        config: Arc::clone(&config),
        events,
        presence: Arc::new(Mutex::new(HashMap::new())),
    };
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    info!(bind = %config.bind, environment = config.environment, "NeuMan Hub listening");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve NeuMan Hub")
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    info!("NeuMan Hub shutdown requested");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        config: HubConfig,
        repo: SqliteRepository,
        admin: Actor,
        project: Project,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = HubConfig::test(temp.path());
        let repo = SqliteRepository::open(&config).expect("repository");
        repo.bootstrap_development_principal("test-token-with-enough-entropy", "Test Admin")
            .expect("bootstrap");
        let admin = repo
            .authenticate("test-token-with-enough-entropy", "studio-a")
            .expect("authenticate");
        let project = repo
            .create_project(
                &admin,
                "project-key-0001",
                &CreateProjectRequest {
                    name: "Test Experience".into(),
                    slug: format!("test-{}", Uuid::new_v4().simple()),
                },
            )
            .expect("create project");
        Fixture {
            _temp: temp,
            config,
            repo,
            admin,
            project,
        }
    }

    fn upload_fixture_object(
        fixture: &Fixture,
        bytes: &[u8],
        media_type: &str,
        suffix: &str,
    ) -> String {
        let hash = content_hash(bytes);
        let negotiation = fixture
            .repo
            .negotiate_upload(
                &fixture.admin,
                &fixture.project.id,
                &format!("neg-{suffix}-0001"),
                &NegotiateUploadRequest {
                    expected_hash: hash.clone(),
                    expected_size: bytes.len() as i64,
                    media_type: media_type.into(),
                },
                &fixture.config.public_base_url,
            )
            .expect("negotiate fixture object");
        fixture
            .repo
            .receive_upload(
                negotiation.upload_id.as_deref().expect("upload id"),
                negotiation
                    .transfer_token
                    .as_deref()
                    .expect("transfer token"),
                &Bytes::copy_from_slice(bytes),
            )
            .expect("upload fixture object");
        fixture
            .repo
            .complete_upload(
                &fixture.admin,
                &fixture.project.id,
                negotiation.upload_id.as_deref().expect("upload id"),
                &format!("complete-{suffix}-0001"),
                &CompleteUploadRequest {
                    purpose: "art-cell".into(),
                    referenced_by: format!("fixture-{suffix}"),
                },
            )
            .expect("complete fixture object");
        hash
    }

    fn canonical_proposal_request(
        fixture: &Fixture,
        cell_id: &str,
        cell_bytes: &[u8],
        suffix: &str,
    ) -> CreateArtProposalRequest {
        let cell_hash = upload_fixture_object(fixture, cell_bytes, CELL_MEDIA_TYPE, suffix);
        let parsed_cell_id: CellId = cell_id.parse().expect("cell id");
        let parsed_hash: ContentHash = cell_hash.parse().expect("cell hash");
        let state = std::collections::BTreeMap::from([(
            parsed_cell_id,
            DomainArtCellState {
                cell_id: parsed_cell_id,
                snapshot_hash: parsed_hash,
                slot_path: "/Workspace/Art".into(),
            },
        )]);
        let state_root = DomainArtRevision::compute_state_root(&state)
            .expect("state root")
            .to_string();
        let manifest = CanonicalArtManifest {
            schema_version: ART_MANIFEST_SCHEMA.into(),
            project_id: fixture.project.id.clone(),
            channel_id: fixture.project.default_channel_id.clone(),
            base_hub_revision_id: None,
            base_state_root: None,
            state_root: state_root.clone(),
            source_session_id: "ses_fixture1234".into(),
            local_revision_id: format!("art_{suffix}1234"),
            changed_cell_ids: vec![cell_id.into()],
            cells: vec![CanonicalArtCell {
                cell_id: cell_id.into(),
                parent_path: "/Workspace/Art".into(),
                content_hash: cell_hash.clone(),
                size_bytes: cell_bytes.len() as u64,
            }],
        };
        let manifest_bytes = serde_jcs::to_vec(&manifest).expect("canonical manifest");
        let manifest_hash = upload_fixture_object(
            fixture,
            &manifest_bytes,
            ART_MANIFEST_MEDIA_TYPE,
            &format!("manifest-{suffix}"),
        );
        let mut object_hashes = vec![cell_hash, manifest_hash];
        object_hashes.sort();
        object_hashes.dedup();
        CreateArtProposalRequest {
            channel_id: fixture.project.default_channel_id.clone(),
            base_revision_id: None,
            state_hash: state_root,
            title: suffix.into(),
            description: String::new(),
            resource_ids: vec![cell_id.into()],
            object_hashes,
        }
    }

    #[test]
    fn development_tokens_are_hashed_and_authentication_is_constant_identity() {
        let fixture = fixture();
        let connection = fixture.repo.conn().expect("connection");
        let stored: String = connection
            .query_row("SELECT token_hash FROM auth_tokens LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("stored token hash");
        assert_ne!(stored, "test-token-with-enough-entropy");
        assert_eq!(stored.len(), 64);
        drop(connection);
        let error = fixture
            .repo
            .authenticate("not-the-token-but-long-enough", "studio-a")
            .expect_err("bad token must fail");
        assert_eq!(error.code, "HUB_AUTHENTICATION_REQUIRED");
    }

    #[test]
    fn project_idempotency_replays_and_rejects_key_reuse() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = HubConfig::test(temp.path());
        let repo = SqliteRepository::open(&config).expect("repository");
        repo.bootstrap_development_principal("test-token-with-enough-entropy", "Admin")
            .expect("bootstrap");
        let actor = repo
            .authenticate("test-token-with-enough-entropy", "desktop")
            .expect("auth");
        let request = CreateProjectRequest {
            name: "Alpha".into(),
            slug: "alpha-project".into(),
        };
        let first = repo
            .create_project(&actor, "same-key-123", &request)
            .expect("first");
        let replay = repo
            .create_project(&actor, "same-key-123", &request)
            .expect("replay");
        assert_eq!(first.id, replay.id);
        let conflict = repo
            .create_project(
                &actor,
                "same-key-123",
                &CreateProjectRequest {
                    name: "Different".into(),
                    slug: "different-project".into(),
                },
            )
            .expect_err("key reuse must conflict");
        assert_eq!(conflict.code, "HUB_IDEMPOTENCY_CONFLICT");
        assert_eq!(repo.list_projects(&actor).expect("projects").len(), 1);
    }

    #[test]
    fn tenant_authorization_precedes_resource_disclosure() {
        let fixture = fixture();
        fixture
            .repo
            .bootstrap_development_principal("second-token-with-enough-entropy", "Outsider")
            .expect("second principal");
        let outsider = fixture
            .repo
            .authenticate("second-token-with-enough-entropy", "desktop-b")
            .expect("authenticate outsider");
        let error = fixture
            .repo
            .get_project(&outsider, &fixture.project.id)
            .expect_err("outsider must not enumerate project");
        assert_eq!(error.code, "HUB_PERMISSION_DENIED");
        let fake_error = fixture
            .repo
            .get_project(&outsider, "prj_does-not-exist")
            .expect_err("fake project must look the same");
        assert_eq!(fake_error.code, error.code);
        assert_eq!(fake_error.status, error.status);
    }

    #[test]
    fn batch_leases_are_atomic_and_conflicts_do_not_partially_insert() {
        let fixture = fixture();
        let first = fixture
            .repo
            .acquire_lease_batch(
                &fixture.admin,
                &fixture.project.id,
                "lease-key-0001",
                &AcquireLeaseBatchRequest {
                    channel_id: fixture.project.default_channel_id.clone(),
                    resource_ids: vec!["cell-a".into(), "cell-b".into()],
                    base_revision_id: None,
                    workstream: "main".into(),
                    intended_action: "edit".into(),
                    cell_hashes: HashMap::new(),
                },
                "leases.acquire_batch",
            )
            .expect("initial batch");
        assert_eq!(first.leases.len(), 2);
        let conflict = fixture
            .repo
            .acquire_lease_batch(
                &fixture.admin,
                &fixture.project.id,
                "lease-key-0002",
                &AcquireLeaseBatchRequest {
                    channel_id: fixture.project.default_channel_id.clone(),
                    resource_ids: vec!["cell-b".into(), "cell-c".into()],
                    base_revision_id: None,
                    workstream: "feature".into(),
                    intended_action: "edit".into(),
                    cell_hashes: HashMap::new(),
                },
                "leases.acquire_batch",
            )
            .expect_err("overlap must conflict");
        assert_eq!(conflict.code, "HUB_LOCK_CONFLICT");
        let leases = fixture
            .repo
            .list_leases(&fixture.admin, &fixture.project.id)
            .expect("leases");
        assert_eq!(leases.len(), 2);
        assert!(!leases.iter().any(|lease| lease.resource_id == "cell-c"));
    }

    #[test]
    fn concurrent_lease_race_has_exactly_one_winner() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = HubConfig::test(temp.path());
        let repo = Arc::new(SqliteRepository::open(&config).expect("repository"));
        repo.bootstrap_development_principal("test-token-with-enough-entropy", "Admin")
            .expect("bootstrap");
        let actor = repo
            .authenticate("test-token-with-enough-entropy", "desktop")
            .expect("authenticate");
        let project = repo
            .create_project(
                &actor,
                "project-race-key",
                &CreateProjectRequest {
                    name: "Lease Race".into(),
                    slug: "lease-race-project".into(),
                },
            )
            .expect("project");
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for index in 0..2 {
            let repo = Arc::clone(&repo);
            let barrier = Arc::clone(&barrier);
            let mut actor = actor.clone();
            actor.session_id = format!("session-{index}");
            let project = project.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                repo.acquire_lease(
                    &actor,
                    &project.id,
                    &format!("race-key-{index:04}"),
                    &AcquireLeaseRequest {
                        channel_id: project.default_channel_id,
                        resource_id: "cell-raced".into(),
                        base_revision_id: None,
                        workstream: format!("worker-{index}"),
                        intended_action: "edit".into(),
                        cell_hash: None,
                    },
                )
            }));
        }
        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter_map(|outcome| outcome.as_ref().err())
                .filter(|error| error.code == "HUB_LOCK_CONFLICT")
                .count(),
            1
        );
    }

    #[test]
    fn renewal_counter_prevents_replay() {
        let fixture = fixture();
        let lease = fixture
            .repo
            .acquire_lease(
                &fixture.admin,
                &fixture.project.id,
                "lease-key-1001",
                &AcquireLeaseRequest {
                    channel_id: fixture.project.default_channel_id.clone(),
                    resource_id: "cell-renew".into(),
                    base_revision_id: None,
                    workstream: "main".into(),
                    intended_action: "edit".into(),
                    cell_hash: None,
                },
            )
            .expect("lease")
            .leases
            .remove(0);
        let request = RenewLeaseRequest {
            renewal_counter: 0,
            base_revision_id: None,
            cell_hash: None,
        };
        let renewed = fixture
            .repo
            .renew_lease(
                &fixture.admin,
                &fixture.project.id,
                &lease.id,
                "renew-key-0001",
                &request,
            )
            .expect("renew");
        assert_eq!(renewed.renewal_counter, 1);
        let replay = fixture
            .repo
            .renew_lease(
                &fixture.admin,
                &fixture.project.id,
                &lease.id,
                "renew-key-0002",
                &request,
            )
            .expect_err("stale counter must fail");
        assert_eq!(replay.code, "HUB_VERSION_CONFLICT");
    }

    #[test]
    fn accepted_head_compare_and_swap_is_atomic() {
        let fixture = fixture();
        let proposal = |key: &str, title: &str| {
            let cell_id = CellId::new().to_string();
            fixture
                .repo
                .acquire_lease(
                    &fixture.admin,
                    &fixture.project.id,
                    &format!("lease-{title}-001"),
                    &AcquireLeaseRequest {
                        channel_id: fixture.project.default_channel_id.clone(),
                        resource_id: cell_id.clone(),
                        base_revision_id: None,
                        workstream: "fixture".into(),
                        intended_action: "art-proposal".into(),
                        cell_hash: None,
                    },
                )
                .expect("lease");
            let request = canonical_proposal_request(&fixture, &cell_id, title.as_bytes(), title);
            fixture
                .repo
                .create_art_proposal(&fixture.admin, &fixture.project.id, key, &request)
                .expect("proposal")
        };
        let first = proposal("proposal-key-001", "first");
        let second = proposal("proposal-key-002", "second");
        let revision = fixture
            .repo
            .accept_art_proposal(
                &fixture.admin,
                &fixture.project.id,
                &first.id,
                "accept-key-0001",
                &AcceptProposalRequest {
                    expected_head_revision_id: None,
                },
            )
            .expect("accept first");
        let stale = fixture
            .repo
            .accept_art_proposal(
                &fixture.admin,
                &fixture.project.id,
                &second.id,
                "accept-key-0002",
                &AcceptProposalRequest {
                    expected_head_revision_id: None,
                },
            )
            .expect_err("stale proposal must fail");
        assert_eq!(stale.code, "HUB_BASE_STALE");
        let head = fixture
            .repo
            .channel_head(
                &fixture.admin,
                &fixture.project.id,
                &fixture.project.default_channel_id,
            )
            .expect("head");
        assert_eq!(head["headRevisionId"], revision.id);
    }

    #[test]
    fn canonical_art_proposal_rejects_state_resource_and_object_omission() {
        let fixture = fixture();
        let cell_id = CellId::new().to_string();
        let request = canonical_proposal_request(&fixture, &cell_id, b"canonical cell", "tamper");
        let mut bad_state = request.clone();
        bad_state.state_hash = ContentHash::digest(b"forged state").to_string();
        assert_eq!(
            fixture
                .repo
                .create_art_proposal(
                    &fixture.admin,
                    &fixture.project.id,
                    "tamper-state-001",
                    &bad_state,
                )
                .expect_err("forged root must fail")
                .code,
            "HUB_ART_STATE_HASH_MISMATCH"
        );

        let mut missing_resource = request.clone();
        missing_resource.resource_ids.clear();
        assert!(
            fixture
                .repo
                .create_art_proposal(
                    &fixture.admin,
                    &fixture.project.id,
                    "tamper-resource-001",
                    &missing_resource,
                )
                .is_err()
        );

        let mut missing_object = request;
        missing_object.object_hashes.pop();
        assert!(
            fixture
                .repo
                .create_art_proposal(
                    &fixture.admin,
                    &fixture.project.id,
                    "tamper-object-001",
                    &missing_object,
                )
                .is_err()
        );
    }

    #[test]
    fn object_round_trip_verifies_hash_and_project_authorization() {
        let fixture = fixture();
        let bytes = Bytes::from_static(b"immutable native art bytes");
        let hash = content_hash(&bytes);
        let negotiation = fixture
            .repo
            .negotiate_upload(
                &fixture.admin,
                &fixture.project.id,
                "upload-key-0001",
                &NegotiateUploadRequest {
                    expected_hash: hash.clone(),
                    expected_size: i64::try_from(bytes.len()).expect("size"),
                    media_type: "application/x-roblox-rbxm".into(),
                },
                &fixture.config.public_base_url,
            )
            .expect("negotiate");
        fixture
            .repo
            .receive_upload(
                negotiation.upload_id.as_deref().expect("upload id"),
                negotiation
                    .transfer_token
                    .as_deref()
                    .expect("transfer token"),
                &bytes,
            )
            .expect("receive");
        let object = fixture
            .repo
            .complete_upload(
                &fixture.admin,
                &fixture.project.id,
                negotiation.upload_id.as_deref().expect("upload id"),
                "complete-key-001",
                &CompleteUploadRequest {
                    purpose: "art-cell".into(),
                    referenced_by: "proposal-draft".into(),
                },
            )
            .expect("complete");
        assert_eq!(object.content_hash, hash);
        let download = fixture
            .repo
            .negotiate_download(
                &fixture.admin,
                &fixture.project.id,
                &hash,
                &fixture.config.public_base_url,
            )
            .expect("download negotiation");
        let (round_trip, _) = fixture
            .repo
            .read_download(&download.download_id, &download.transfer_token)
            .expect("download");
        assert_eq!(round_trip, bytes);

        fixture
            .repo
            .bootstrap_development_principal("outsider-token-long-enough", "Outsider")
            .expect("outsider");
        let outsider = fixture
            .repo
            .authenticate("outsider-token-long-enough", "other")
            .expect("auth outsider");
        let denied = fixture
            .repo
            .object_metadata(&outsider, &fixture.project.id, &hash)
            .expect_err("outsider object read");
        assert_eq!(denied.code, "HUB_PERMISSION_DENIED");
    }

    #[test]
    fn signed_event_cursor_detects_tampering() {
        let fixture = fixture();
        let cursor = sign_cursor(42, &fixture.config);
        assert_eq!(
            parse_cursor(Some(&cursor), &fixture.config).expect("valid cursor"),
            42
        );
        let mut tampered = cursor.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'a' { b'b' } else { b'a' };
        let tampered = String::from_utf8(tampered).expect("utf8");
        assert!(parse_cursor(Some(&tampered), &fixture.config).is_err());
    }

    #[test]
    fn versioned_router_constructs_with_all_contract_routes() {
        let fixture = fixture();
        let (events, _) = broadcast::channel(16);
        let state = AppState {
            repo: Arc::new(fixture.repo),
            config: Arc::new(fixture.config),
            events,
            presence: Arc::new(Mutex::new(HashMap::new())),
        };
        let _router = build_router(state);
    }
}
