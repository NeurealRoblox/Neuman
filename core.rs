//! Local orchestration services for NeuMan.
//!
//! This module deliberately separates pure domain identity from filesystem,
//! SQLite, Git, OAuth, and Roblox provider effects. All external mutations return
//! durable receipts and all artifact writes are immutable.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::domain::{
    ArtCellState, ArtRevision, ArtRevisionId, ArtRevisionStatus, BuildId, BundleArtifact, CellId,
    ContentHash, GitObjectFormat, GitOid, LogicalBuildInput, ProjectId, ReleaseBundleManifest,
    ReleaseId, ReleaseStatus, Reproducibility, RobloxId, Sha256Hash, canonical_json,
    hash_canonical,
};

/// Stable core failure with a public code and retry classification.
#[derive(Debug, Error)]
#[error("{code}: {message}")]
pub struct CoreError {
    /// Versioned public error code.
    pub code: &'static str,
    /// User-safe message.
    pub message: String,
    /// Whether retrying unchanged input may succeed.
    pub retryable: bool,
}

impl CoreError {
    /// Creates a non-retryable public error.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    /// Creates a retryable public error.
    pub fn retryable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: true,
        }
    }
}

type Result<T> = std::result::Result<T, CoreError>;

fn io_error(code: &'static str, error: impl std::fmt::Display) -> CoreError {
    CoreError::new(code, error.to_string())
}

/// Parsed `neuman.project.yaml` root.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectManifest {
    /// Manifest schema major/minor.
    pub schema_version: String,
    /// Human project metadata.
    pub project: ProjectConfig,
    /// Git authority configuration.
    pub repository: RepositoryConfig,
    /// Pinned tool constraints.
    pub toolchain: ToolchainConfig,
    /// External provider configuration without credentials.
    pub providers: ProviderConfig,
    /// Art channels by stable key.
    #[serde(default)]
    pub art_channels: BTreeMap<String, ArtChannelConfig>,
    /// Roblox places by stable key.
    pub places: BTreeMap<String, PlaceConfig>,
    /// Deployment environments by stable key.
    pub environments: BTreeMap<String, EnvironmentConfig>,
    /// Named policy values. Schemas are interpreted by the owning service.
    pub policies: BTreeMap<String, Value>,
    /// Named validation profile values.
    #[serde(default)]
    pub validation: BTreeMap<String, Value>,
    /// Namespaced forward-compatible extension values.
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

/// Project metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectConfig {
    /// URL/path-safe project slug.
    pub slug: String,
    /// Human name.
    pub display_name: String,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// Explicit nested repository permission.
    #[serde(default)]
    pub allow_nested: bool,
    /// Default place key.
    #[serde(default)]
    pub default_place: Option<String>,
    /// Default art channel key.
    #[serde(default)]
    pub default_art_channel: Option<String>,
}

/// Git repository configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryConfig {
    /// `github`, `generic-git`, or `local`.
    pub provider: String,
    /// Credential-free canonical remote.
    #[serde(default)]
    pub remote: Option<String>,
    /// Stable GitHub numeric repository ID.
    #[serde(default)]
    pub github_repository_id: Option<String>,
    /// Default branch.
    pub default_branch: String,
    /// Release branch.
    #[serde(default)]
    pub release_branch: Option<String>,
    /// Git object format.
    #[serde(default = "default_object_format")]
    pub object_format: GitObjectFormat,
    /// Rojo project file.
    pub project_file: String,
    /// Require clean worktree for build.
    #[serde(default = "yes")]
    pub require_clean_worktree_for_build: bool,
    /// Whether explicitly approved submodules are allowed.
    #[serde(default)]
    pub allow_submodules: bool,
    /// Whether NeuMan may invoke Git hooks. Defaults false.
    #[serde(default)]
    pub allow_git_hooks: bool,
}

fn default_object_format() -> GitObjectFormat {
    GitObjectFormat::Sha1
}
fn yes() -> bool {
    true
}

/// Toolchain constraint block retained as typed arbitrary data.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainConfig {
    /// NeuMan version constraint.
    pub neuman: String,
    /// Rojo constraint/configuration.
    pub rojo: Value,
    /// Studio constraint/configuration.
    #[serde(default)]
    pub studio: Value,
    /// API schema constraint/configuration.
    #[serde(default)]
    pub api_schema: Value,
    /// Optional pinned helpers.
    #[serde(default)]
    pub helpers: BTreeMap<String, Value>,
}

/// External providers. Values cannot contain secret-like keys.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConfig {
    /// Art object store.
    pub art_store: Value,
    /// Optional Hub endpoint/project ID.
    #[serde(default)]
    pub hub: Option<Value>,
    /// Optional GitHub App metadata.
    #[serde(default)]
    pub git_hub: Option<Value>,
    /// Roblox public OAuth client metadata.
    #[serde(default)]
    pub roblox: Option<Value>,
}

/// Art channel policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtChannelConfig {
    /// Human name.
    pub display_name: String,
    /// Protected channels require an approval ledger.
    #[serde(default)]
    pub protected: bool,
    /// Place key used to author the art.
    pub authoring_place: String,
    /// Named acceptance policy.
    pub acceptance_policy: String,
    /// Lock mode.
    pub lock_policy: String,
    /// Draft capture while offline.
    #[serde(default)]
    pub allow_offline_capture: bool,
    /// Acceptance while offline; normally false.
    #[serde(default)]
    pub allow_offline_acceptance: bool,
}

/// Environment safety metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentConfig {
    /// Environment kind.
    pub kind: String,
    /// Activates high-impact release rules.
    #[serde(default)]
    pub production_impact: bool,
    /// Environments whose proof must precede this environment.
    #[serde(default)]
    pub required_for: Vec<String>,
    /// Approval policy name.
    #[serde(default)]
    pub approvals: Option<String>,
}

/// Place topology and ownership.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlaceConfig {
    /// Human name.
    pub display_name: String,
    /// Immutable base template reference.
    pub base_template: BaseTemplate,
    /// Optional authoring target.
    #[serde(default)]
    pub authoring: Option<RobloxTarget>,
    /// Deployment targets keyed by environment.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,
    /// Complete ownership declaration.
    pub ownership: Vec<OwnershipConfig>,
    /// Validation profile name.
    pub validation_profile: String,
    /// Release policy name.
    pub release_policy: String,
}

/// Base template object.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseTemplate {
    /// `repository-file` in v1.
    #[serde(rename = "type")]
    pub kind: String,
    /// Repository-relative path.
    pub path: String,
    /// Expected SHA-256 identity.
    #[serde(default)]
    pub sha256: Option<Sha256Hash>,
}

/// Roblox universe/place tuple.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RobloxTarget {
    /// Universe ID serialized as a string.
    pub universe_id: RobloxId,
    /// Place ID serialized as a string.
    pub place_id: RobloxId,
    /// Creator descriptor.
    pub creator: Creator,
}

/// Roblox creator identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Creator {
    /// `user` or `group`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Creator numeric ID.
    pub id: RobloxId,
}

/// Target plus publication mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetConfig {
    /// Universe ID.
    pub universe_id: RobloxId,
    /// Place ID.
    pub place_id: RobloxId,
    /// Creator descriptor.
    pub creator: Creator,
    /// `studio-assisted`, `open-cloud`, or `manual-handoff`.
    pub publication: String,
}

/// One DataModel ownership root.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipConfig {
    /// Stable manifest-local identity.
    pub id: String,
    /// Absolute escaped DataModel path.
    pub path: String,
    /// Authority kind.
    pub owner: String,
    /// Allows separately declared child roots.
    #[serde(default)]
    pub allow_owned_descendants: bool,
    /// Handling for undeclared descendants.
    #[serde(default)]
    pub unknown_instances: Option<String>,
    /// Owner-specific options.
    #[serde(flatten)]
    pub options: BTreeMap<String, Value>,
}

/// Full multi-error validation report.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    /// Whether there are no errors.
    pub valid: bool,
    /// Independent errors found.
    pub errors: Vec<ValidationIssue>,
    /// Non-blocking warnings.
    pub warnings: Vec<ValidationIssue>,
    /// Canonical effective manifest hash, when parsing succeeded.
    pub manifest_hash: Option<ContentHash>,
}

/// One actionable manifest issue.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    /// Stable code.
    pub code: String,
    /// JSON/YAML-like field path.
    pub path: String,
    /// User-safe explanation.
    pub message: String,
}

impl ProjectManifest {
    /// Parses a bounded YAML manifest and validates all discoverable invariants.
    pub fn parse(bytes: &[u8]) -> std::result::Result<(Self, ValidationReport), ValidationReport> {
        let mut parse_errors = Vec::new();
        if bytes.len() > 1_048_576 {
            parse_errors.push(issue("MANIFEST_TOO_LARGE", "$", "manifest exceeds 1 MiB"));
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            for (line_number, line) in text.lines().enumerate() {
                let code = line.split('#').next().unwrap_or_default();
                if code
                    .split_whitespace()
                    .any(|token| token.starts_with('&') || token.starts_with('*'))
                {
                    parse_errors.push(issue(
                        "YAML_ALIAS_FORBIDDEN",
                        format!("line {}", line_number + 1),
                        "YAML anchors and aliases are forbidden",
                    ));
                }
            }
        }
        if !parse_errors.is_empty() {
            return Err(ValidationReport {
                valid: false,
                errors: parse_errors,
                warnings: vec![],
                manifest_hash: None,
            });
        }
        let manifest: Self = match serde_yaml::from_slice(bytes) {
            Ok(value) => value,
            Err(error) => {
                return Err(ValidationReport {
                    valid: false,
                    errors: vec![issue("MANIFEST_PARSE_FAILED", "$", error.to_string())],
                    warnings: vec![],
                    manifest_hash: None,
                });
            }
        };
        let report = manifest.validate();
        if report.valid {
            Ok((manifest, report))
        } else {
            Err(report)
        }
    }

    /// Reads `neuman.project.yaml` from a project root.
    pub fn load(root: &Path) -> std::result::Result<(Self, ValidationReport), ValidationReport> {
        match fs::read(root.join("neuman.project.yaml")) {
            Ok(bytes) => Self::parse(&bytes),
            Err(error) => Err(ValidationReport {
                valid: false,
                errors: vec![issue(
                    "MANIFEST_READ_FAILED",
                    "neuman.project.yaml",
                    error.to_string(),
                )],
                warnings: vec![],
                manifest_hash: None,
            }),
        }
    }

    /// Performs cross-field validation without external provider access.
    #[must_use]
    pub fn validate(&self) -> ValidationReport {
        let mut errors = Vec::new();
        let warnings = Vec::new();
        if self.schema_version != "1.0" {
            errors.push(issue(
                "SCHEMA_UNSUPPORTED",
                "schemaVersion",
                "only schemaVersion 1.0 is supported",
            ));
        }
        if !valid_slug(&self.project.slug) {
            errors.push(issue(
                "PROJECT_SLUG_INVALID",
                "project.slug",
                "slug must be 3..63 lowercase letters, digits, or internal hyphens",
            ));
        }
        if self.project.display_name.is_empty() || self.project.display_name.chars().count() > 100 {
            errors.push(issue(
                "PROJECT_NAME_INVALID",
                "project.displayName",
                "display name must contain 1..100 characters",
            ));
        }
        if let Some(place) = &self.project.default_place
            && !self.places.contains_key(place)
        {
            errors.push(issue(
                "DEFAULT_PLACE_UNKNOWN",
                "project.defaultPlace",
                "default place does not exist",
            ));
        }
        if let Some(channel) = &self.project.default_art_channel
            && !self.art_channels.contains_key(channel)
        {
            errors.push(issue(
                "DEFAULT_ART_CHANNEL_UNKNOWN",
                "project.defaultArtChannel",
                "default art channel does not exist",
            ));
        }
        if self.repository.allow_git_hooks {
            errors.push(issue(
                "GIT_HOOKS_FORBIDDEN",
                "repository.allowGitHooks",
                "repository-supplied hooks are not supported by this implementation",
            ));
        }
        if let Some(remote) = &self.repository.remote {
            match Url::parse(remote) {
                Ok(url)
                    if url.username().is_empty()
                        && url.password().is_none()
                        && matches!(url.scheme(), "https" | "ssh") => {}
                _ => errors.push(issue(
                    "GIT_REMOTE_UNSAFE",
                    "repository.remote",
                    "remote must use https/ssh and contain no embedded credentials",
                )),
            }
        }
        if is_absolute_or_parent_escape(&self.repository.project_file) {
            errors.push(issue(
                "REPOSITORY_PATH_UNSAFE",
                "repository.projectFile",
                "path must remain inside the repository",
            ));
        }
        reject_secret_values(
            "providers",
            &serde_json::to_value(&self.providers).unwrap_or(Value::Null),
            &mut errors,
        );

        let mut target_ids = BTreeSet::new();
        for (place_key, place) in &self.places {
            if !valid_key(place_key) {
                errors.push(issue(
                    "PLACE_KEY_INVALID",
                    format!("places.{place_key}"),
                    "place key is invalid",
                ));
            }
            if is_absolute_or_parent_escape(&place.base_template.path) {
                errors.push(issue(
                    "BASE_TEMPLATE_PATH_UNSAFE",
                    format!("places.{place_key}.baseTemplate.path"),
                    "path must remain inside repository",
                ));
            }
            if !self.policies.contains_key(&place.release_policy) {
                errors.push(issue(
                    "RELEASE_POLICY_UNKNOWN",
                    format!("places.{place_key}.releasePolicy"),
                    "named release policy is missing",
                ));
            }
            let mut roots = place.ownership.clone();
            roots.sort_by(|a, b| {
                normalize_data_model_path(&a.path).cmp(&normalize_data_model_path(&b.path))
            });
            let mut root_ids = BTreeSet::new();
            for root in &roots {
                if !root_ids.insert(&root.id) {
                    errors.push(issue(
                        "OWNERSHIP_ID_DUPLICATE",
                        format!("places.{place_key}.ownership"),
                        format!("duplicate ownership id {}", root.id),
                    ));
                }
                if normalize_data_model_path(&root.path).is_none() {
                    errors.push(issue(
                        "OWNERSHIP_PATH_INVALID",
                        format!("places.{place_key}.ownership.{}.path", root.id),
                        "path must be absolute and use valid ~0/~1 escapes",
                    ));
                }
                if !matches!(
                    root.owner.as_str(),
                    "git-code"
                        | "studio-art"
                        | "terrain"
                        | "service-state"
                        | "generated"
                        | "external-package"
                ) {
                    errors.push(issue(
                        "OWNERSHIP_OWNER_INVALID",
                        format!("places.{place_key}.ownership.{}.owner", root.id),
                        "unknown owner kind",
                    ));
                }
            }
            for i in 0..roots.len() {
                for child in roots.iter().skip(i + 1) {
                    let parent = &roots[i];
                    if ownership_overlaps(&parent.path, &child.path) {
                        let legal_delegation = parent.path != child.path
                            && parent.allow_owned_descendants
                            && parent.unknown_instances.as_deref() != Some("capture");
                        if !legal_delegation {
                            errors.push(issue(
                                "OWNERSHIP_OVERLAP",
                                format!("places.{place_key}.ownership"),
                                format!("{} overlaps {}", parent.id, child.id),
                            ));
                        }
                    }
                }
            }
            if let Some(authoring) = &place.authoring {
                target_ids.insert((
                    authoring.universe_id.to_string(),
                    authoring.place_id.to_string(),
                    format!("{place_key}.authoring"),
                ));
            }
            for (environment, target) in &place.targets {
                if !self.environments.contains_key(environment) {
                    errors.push(issue(
                        "TARGET_ENVIRONMENT_UNKNOWN",
                        format!("places.{place_key}.targets.{environment}"),
                        "environment is not declared",
                    ));
                }
                if !matches!(
                    target.publication.as_str(),
                    "studio-assisted" | "open-cloud" | "manual-handoff"
                ) {
                    errors.push(issue(
                        "PUBLICATION_MODE_INVALID",
                        format!("places.{place_key}.targets.{environment}.publication"),
                        "unknown publication mode",
                    ));
                }
                target_ids.insert((
                    target.universe_id.to_string(),
                    target.place_id.to_string(),
                    format!("{place_key}.{environment}"),
                ));
                if let Some(authoring) = &place.authoring
                    && authoring.universe_id == target.universe_id
                    && authoring.place_id == target.place_id
                    && self
                        .environments
                        .get(environment)
                        .is_some_and(|env| env.production_impact)
                {
                    errors.push(issue(
                        "PRODUCTION_IS_AUTHORING",
                        format!("places.{place_key}.targets.{environment}"),
                        "production-impact target cannot be the authoring target",
                    ));
                }
            }
        }
        let mut by_target: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for (universe, place, location) in target_ids {
            by_target
                .entry((universe, place))
                .or_default()
                .push(location);
        }
        for (target, locations) in by_target {
            if locations.len() > 1 {
                errors.push(issue(
                    "TARGET_DUPLICATE",
                    "places",
                    format!(
                        "target {}/{} appears at {}",
                        target.0,
                        target.1,
                        locations.join(", ")
                    ),
                ));
            }
        }
        for (key, channel) in &self.art_channels {
            if !self.places.contains_key(&channel.authoring_place) {
                errors.push(issue(
                    "ART_AUTHORING_PLACE_UNKNOWN",
                    format!("artChannels.{key}.authoringPlace"),
                    "place does not exist",
                ));
            }
            if channel.protected && channel.allow_offline_acceptance {
                errors.push(issue(
                    "PROTECTED_OFFLINE_ACCEPTANCE",
                    format!("artChannels.{key}"),
                    "protected channels cannot accept revisions offline",
                ));
            }
        }
        let manifest_hash = hash_canonical("neuman-project-manifest-v1\0", self).ok();
        ValidationReport {
            valid: errors.is_empty(),
            errors,
            warnings,
            manifest_hash,
        }
    }
}

fn issue(
    code: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        code: code.into(),
        path: path.into(),
        message: message.into(),
    }
}

fn valid_slug(value: &str) -> bool {
    value.len() >= 3
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_key(value: &str) -> bool {
    value.len() >= 2
        && value.len() <= 32
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_absolute_or_parent_escape(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
}

fn normalize_data_model_path(value: &str) -> Option<String> {
    if !value.starts_with('/') || value.ends_with('/') || value.contains("//") {
        return None;
    }
    for segment in value[1..].split('/') {
        if segment.is_empty() {
            return None;
        }
        let bytes = segment.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'~' {
                if i + 1 >= bytes.len() || !matches!(bytes[i + 1], b'0' | b'1') {
                    return None;
                }
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    Some(value.to_string())
}

fn ownership_overlaps(left: &str, right: &str) -> bool {
    left == right
        || right
            .strip_prefix(left)
            .is_some_and(|rest| rest.starts_with('/'))
        || left
            .strip_prefix(right)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn reject_secret_values(path: &str, value: &Value, errors: &mut Vec<ValidationIssue>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if [
                    "secret",
                    "token",
                    "password",
                    "apikey",
                    "api_key",
                    "cookie",
                    ".roblosecurity",
                ]
                .iter()
                .any(|term| key.to_ascii_lowercase().contains(term))
                {
                    errors.push(issue(
                        "SECRET_IN_MANIFEST",
                        &child_path,
                        "credential-like provider key is forbidden",
                    ));
                }
                reject_secret_values(&child_path, child, errors);
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                reject_secret_values(&format!("{path}[{index}]"), child, errors);
            }
        }
        _ => {}
    }
}

/// Discovers a project manifest without walking across a Git repository boundary.
pub fn discover_project(start: &Path) -> Result<PathBuf> {
    let mut current =
        fs::canonicalize(start).map_err(|error| io_error("PROJECT_PATH_INVALID", error))?;
    if current.is_file() {
        current.pop();
    }
    loop {
        if current.join("neuman.project.yaml").is_file() {
            return Ok(current);
        }
        if current.join(".git").exists() {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    Err(CoreError::new(
        "PROJECT_NOT_FOUND",
        "no neuman.project.yaml found before repository boundary",
    ))
}

/// Local immutable metadata ledger. Each method opens a short SQLite connection,
/// making the handle safe to share across async tasks.
#[derive(Clone, Debug)]
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    /// Opens or initializes the ledger schema.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("LEDGER_DIRECTORY_FAILED", error))?;
        }
        let ledger = Self { path };
        let connection = ledger.connect()?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT OR IGNORE INTO meta(key,value) VALUES('schema_version','1');
             CREATE TABLE IF NOT EXISTS art_revisions(id TEXT PRIMARY KEY, project_id TEXT NOT NULL, channel_id TEXT NOT NULL, state_root TEXT NOT NULL, status TEXT NOT NULL, canonical_json BLOB NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS builds(id TEXT PRIMARY KEY, logical_hash TEXT NOT NULL UNIQUE, status TEXT NOT NULL, input_json BLOB NOT NULL, bundle_hash TEXT, created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS bundles(hash TEXT PRIMARY KEY, manifest_json BLOB NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS releases(id TEXT PRIMARY KEY, bundle_hash TEXT NOT NULL, environment TEXT NOT NULL, status TEXT NOT NULL, request_json BLOB NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS release_receipts(receipt_id TEXT PRIMARY KEY, release_id TEXT NOT NULL, phase TEXT NOT NULL, receipt_json BLOB NOT NULL, created_at TEXT NOT NULL, FOREIGN KEY(release_id) REFERENCES releases(id));
             CREATE TABLE IF NOT EXISTS studio_transfers(session_id TEXT NOT NULL, transfer_id TEXT NOT NULL, content_hash TEXT NOT NULL, size_bytes INTEGER NOT NULL, created_at TEXT NOT NULL, consumed_by_revision TEXT, PRIMARY KEY(session_id,transfer_id));
             CREATE TABLE IF NOT EXISTS studio_capture_receipts(session_id TEXT NOT NULL, transfer_id TEXT NOT NULL, revision_id TEXT NOT NULL UNIQUE, mutation_epoch INTEGER NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY(session_id,transfer_id), FOREIGN KEY(revision_id) REFERENCES art_revisions(id));
             CREATE TABLE IF NOT EXISTS hub_revision_receipts(remote_authority_id TEXT NOT NULL, remote_revision_id TEXT NOT NULL, event_id TEXT NOT NULL UNIQUE, local_revision_id TEXT NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY(remote_authority_id,remote_revision_id), FOREIGN KEY(local_revision_id) REFERENCES art_revisions(id));
             CREATE TABLE IF NOT EXISTS hub_stream_cursors(remote_authority_id TEXT PRIMARY KEY, sequence INTEGER NOT NULL, cursor TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS art_channel_heads(project_id TEXT NOT NULL, channel_id TEXT NOT NULL, revision_id TEXT NOT NULL, state_root TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(project_id,channel_id), FOREIGN KEY(revision_id) REFERENCES art_revisions(id));",
        ).map_err(|error| io_error("LEDGER_SCHEMA_FAILED", error))?;
        Ok(ledger)
    }

    fn connect(&self) -> Result<Connection> {
        let connection =
            Connection::open(&self.path).map_err(|error| io_error("LEDGER_OPEN_FAILED", error))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| io_error("LEDGER_BUSY_CONFIG_FAILED", error))?;
        Ok(connection)
    }

    /// Inserts immutable art metadata. Repeating identical content is idempotent;
    /// reusing an ID for different bytes is corruption.
    pub fn put_art_revision(&self, revision: &ArtRevision) -> Result<()> {
        revision
            .validate()
            .map_err(|error| CoreError::new("ART_REVISION_INVALID", error.to_string()))?;
        let bytes = canonical_json(revision)
            .map_err(|error| CoreError::new("ART_REVISION_SERIALIZE_FAILED", error.to_string()))?;
        let connection = self.connect()?;
        let existing: Option<Vec<u8>> = connection
            .query_row(
                "SELECT canonical_json FROM art_revisions WHERE id=?1",
                [revision.art_revision_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        if let Some(existing) = existing {
            return if existing == bytes {
                Ok(())
            } else {
                Err(CoreError::new(
                    "ART_REVISION_IMMUTABLE",
                    "revision ID already has different canonical metadata",
                ))
            };
        }
        connection.execute("INSERT INTO art_revisions(id,project_id,channel_id,state_root,status,canonical_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![revision.art_revision_id.to_string(), revision.project_id.to_string(), revision.channel_id, revision.state_root_hash.to_string(), serde_json::to_string(&revision.status).unwrap_or_default().trim_matches('"'), bytes, revision.created_at]).map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    /// Loads art metadata by ID.
    pub fn art_revision(&self, id: ArtRevisionId) -> Result<Option<ArtRevision>> {
        self.read_json(
            "SELECT canonical_json FROM art_revisions WHERE id=?1",
            &id.to_string(),
        )
    }

    /// Lists art revisions newest first.
    pub fn art_revisions(&self) -> Result<Vec<ArtRevision>> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare("SELECT canonical_json FROM art_revisions ORDER BY created_at DESC, id DESC")
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        rows.map(|row| decode_json(&row.map_err(|error| io_error("LEDGER_READ_FAILED", error))?))
            .collect()
    }

    /// Returns the immutable accepted head for one project/channel, if present.
    pub fn accepted_art_head(
        &self,
        project_id: ProjectId,
        channel: &str,
    ) -> Result<Option<ArtRevision>> {
        let connection = self.connect()?;
        let revision_id: Option<String> = connection
            .query_row(
                "SELECT revision_id FROM art_channel_heads WHERE project_id=?1 AND channel_id=?2",
                params![project_id.to_string(), channel],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        let revision_id = if revision_id.is_some() {
            revision_id
        } else {
            connection
                .query_row(
                    "SELECT id FROM art_revisions WHERE project_id=?1 AND channel_id=?2 AND status='accepted' ORDER BY created_at DESC,id DESC LIMIT 1",
                    params![project_id.to_string(), channel],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| io_error("LEDGER_READ_FAILED", error))?
        };
        let Some(revision_id) = revision_id else {
            return Ok(None);
        };
        let revision_id = ArtRevisionId::from_str(&revision_id)
            .map_err(|error| CoreError::new("LEDGER_CORRUPT", error.to_string()))?;
        self.art_revision(revision_id)
    }

    fn put_studio_transfer(&self, transfer: &StudioTransferRecord) -> Result<()> {
        let connection = self.connect()?;
        let existing: Option<(String, u64)> = connection
            .query_row(
                "SELECT content_hash,size_bytes FROM studio_transfers WHERE session_id=?1 AND transfer_id=?2",
                params![transfer.session_id, transfer.transfer_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        if let Some((content_hash, size_bytes)) = existing {
            return if content_hash == transfer.content_hash.to_string()
                && size_bytes == transfer.size_bytes
            {
                Ok(())
            } else {
                Err(CoreError::new(
                    "STUDIO_TRANSFER_IMMUTABLE",
                    "the Studio transfer ID already names different content",
                ))
            };
        }
        connection
            .execute(
                "INSERT INTO studio_transfers(session_id,transfer_id,content_hash,size_bytes,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![
                    transfer.session_id,
                    transfer.transfer_id,
                    transfer.content_hash.to_string(),
                    transfer.size_bytes,
                    transfer.created_at
                ],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    fn studio_transfer(
        &self,
        session_id: &str,
        transfer_id: &str,
    ) -> Result<Option<StudioTransferRecord>> {
        self.connect()?
            .query_row(
                "SELECT content_hash,size_bytes,created_at FROM studio_transfers WHERE session_id=?1 AND transfer_id=?2",
                params![session_id, transfer_id],
                |row| {
                    let hash: String = row.get(0)?;
                    let size_bytes: u64 = row.get(1)?;
                    let created_at: String = row.get(2)?;
                    Ok((hash, size_bytes, created_at))
                },
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?
            .map(|(hash, size_bytes, created_at)| {
                Ok(StudioTransferRecord {
                    session_id: session_id.to_owned(),
                    transfer_id: transfer_id.to_owned(),
                    content_hash: ContentHash::from_str(&hash).map_err(|error| {
                        CoreError::new("LEDGER_CORRUPT", error.to_string())
                    })?,
                    size_bytes,
                    created_at,
                })
            })
            .transpose()
    }

    fn studio_capture_revision(
        &self,
        session_id: &str,
        transfer_id: &str,
    ) -> Result<Option<ArtRevision>> {
        let id: Option<String> = self
            .connect()?
            .query_row(
                "SELECT revision_id FROM studio_capture_receipts WHERE session_id=?1 AND transfer_id=?2",
                params![session_id, transfer_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        let Some(id) = id else {
            return Ok(None);
        };
        let id = ArtRevisionId::from_str(&id)
            .map_err(|error| CoreError::new("LEDGER_CORRUPT", error.to_string()))?;
        self.art_revision(id)
    }

    fn commit_studio_capture(
        &self,
        revision: &ArtRevision,
        session_id: &str,
        transfer_id: &str,
        mutation_epoch: u64,
        expected_head: Option<ArtRevisionId>,
    ) -> Result<()> {
        revision
            .validate()
            .map_err(|error| CoreError::new("ART_REVISION_INVALID", error.to_string()))?;
        let bytes = canonical_json(revision)
            .map_err(|error| CoreError::new("ART_REVISION_SERIALIZE_FAILED", error.to_string()))?;
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT revision_id FROM studio_capture_receipts WHERE session_id=?1 AND transfer_id=?2",
                params![session_id, transfer_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        if let Some(existing) = existing {
            return if existing == revision.art_revision_id.to_string() {
                Ok(())
            } else {
                Err(CoreError::new("STUDIO_CAPTURE_ALREADY_COMMITTED", existing))
            };
        }
        let transfer_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM studio_transfers WHERE session_id=?1 AND transfer_id=?2 AND consumed_by_revision IS NULL)",
                params![session_id, transfer_id],
                |row| row.get(0),
            )
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        if !transfer_exists {
            return Err(CoreError::new("STUDIO_TRANSFER_NOT_AVAILABLE", transfer_id));
        }
        if revision.status == ArtRevisionStatus::Accepted {
            let mut current: Option<String> = transaction
                .query_row(
                    "SELECT revision_id FROM art_channel_heads WHERE project_id=?1 AND channel_id=?2",
                    params![revision.project_id.to_string(), revision.channel_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
            if current.is_none() {
                current = transaction
                    .query_row(
                        "SELECT id FROM art_revisions WHERE project_id=?1 AND channel_id=?2 AND status='accepted' ORDER BY created_at DESC,id DESC LIMIT 1",
                        params![revision.project_id.to_string(), revision.channel_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
            }
            let expected = expected_head.map(|id| id.to_string());
            if current != expected {
                return Err(CoreError::new(
                    "ART_HEAD_CONFLICT",
                    format!("expected {:?}, found {:?}", expected, current),
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO art_revisions(id,project_id,channel_id,state_root,status,canonical_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    revision.art_revision_id.to_string(),
                    revision.project_id.to_string(),
                    revision.channel_id,
                    revision.state_root_hash.to_string(),
                    art_status_text(revision.status),
                    bytes,
                    revision.created_at
                ],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        if revision.status == ArtRevisionStatus::Accepted {
            transaction
                .execute(
                    "INSERT INTO art_channel_heads(project_id,channel_id,revision_id,state_root,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(project_id,channel_id) DO UPDATE SET revision_id=excluded.revision_id,state_root=excluded.state_root,updated_at=excluded.updated_at",
                    params![
                        revision.project_id.to_string(),
                        revision.channel_id,
                        revision.art_revision_id.to_string(),
                        revision.state_root_hash.to_string(),
                        revision.created_at
                    ],
                )
                .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        }
        transaction
            .execute(
                "INSERT INTO studio_capture_receipts(session_id,transfer_id,revision_id,mutation_epoch,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![
                    session_id,
                    transfer_id,
                    revision.art_revision_id.to_string(),
                    mutation_epoch,
                    revision.created_at
                ],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        let consumed = transaction
            .execute(
                "UPDATE studio_transfers SET consumed_by_revision=?1 WHERE session_id=?2 AND transfer_id=?3 AND consumed_by_revision IS NULL",
                params![revision.art_revision_id.to_string(), session_id, transfer_id],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        if consumed != 1 {
            return Err(CoreError::new(
                "STUDIO_TRANSFER_CONSUME_CONFLICT",
                transfer_id,
            ));
        }
        transaction
            .commit()
            .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
        Ok(())
    }

    fn hub_revision_local_id(
        &self,
        remote_authority_id: &str,
        remote_revision_id: &str,
    ) -> Result<Option<ArtRevisionId>> {
        let id: Option<String> = self
            .connect()?
            .query_row(
                "SELECT local_revision_id FROM hub_revision_receipts WHERE remote_authority_id=?1 AND remote_revision_id=?2",
                params![remote_authority_id, remote_revision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        id.map(|value| {
            ArtRevisionId::from_str(&value)
                .map_err(|error| CoreError::new("LEDGER_CORRUPT", error.to_string()))
        })
        .transpose()
    }

    fn commit_hub_revision(
        &self,
        revision: &ArtRevision,
        remote_authority_id: &str,
        remote_revision_id: &str,
        event_id: &str,
        expected_head: Option<ArtRevisionId>,
    ) -> Result<ArtRevisionId> {
        revision
            .validate()
            .map_err(|error| CoreError::new("ART_REVISION_INVALID", error.to_string()))?;
        if revision.status != ArtRevisionStatus::Accepted {
            return Err(CoreError::new(
                "HUB_REVISION_INVALID",
                "remote channel heads must import as accepted revisions",
            ));
        }
        let bytes = canonical_json(revision)
            .map_err(|error| CoreError::new("ART_REVISION_SERIALIZE_FAILED", error.to_string()))?;
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT local_revision_id FROM hub_revision_receipts WHERE remote_authority_id=?1 AND remote_revision_id=?2",
                params![remote_authority_id, remote_revision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        if let Some(existing) = existing {
            transaction
                .commit()
                .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
            return ArtRevisionId::from_str(&existing)
                .map_err(|error| CoreError::new("LEDGER_CORRUPT", error.to_string()));
        }
        let current: Option<String> = transaction
            .query_row(
                "SELECT revision_id FROM art_channel_heads WHERE project_id=?1 AND channel_id=?2",
                params![revision.project_id.to_string(), revision.channel_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        let expected = expected_head.map(|value| value.to_string());
        if current != expected {
            return Err(CoreError::new(
                "HUB_HEAD_CONFLICT",
                format!("expected {:?}, found {:?}", expected, current),
            ));
        }
        transaction
            .execute(
                "INSERT INTO art_revisions(id,project_id,channel_id,state_root,status,canonical_json,created_at) VALUES(?1,?2,?3,?4,'accepted',?5,?6)",
                params![
                    revision.art_revision_id.to_string(),
                    revision.project_id.to_string(),
                    revision.channel_id,
                    revision.state_root_hash.to_string(),
                    bytes,
                    revision.created_at
                ],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        transaction
            .execute(
                "INSERT INTO art_channel_heads(project_id,channel_id,revision_id,state_root,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(project_id,channel_id) DO UPDATE SET revision_id=excluded.revision_id,state_root=excluded.state_root,updated_at=excluded.updated_at",
                params![
                    revision.project_id.to_string(),
                    revision.channel_id,
                    revision.art_revision_id.to_string(),
                    revision.state_root_hash.to_string(),
                    revision.created_at
                ],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        transaction
            .execute(
                "INSERT INTO hub_revision_receipts(remote_authority_id,remote_revision_id,event_id,local_revision_id,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![
                    remote_authority_id,
                    remote_revision_id,
                    event_id,
                    revision.art_revision_id.to_string(),
                    revision.created_at
                ],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        transaction
            .commit()
            .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
        Ok(revision.art_revision_id)
    }

    fn hub_stream_cursor(&self, remote_authority_id: &str) -> Result<Option<(i64, String)>> {
        self.connect()?
            .query_row(
                "SELECT sequence,cursor FROM hub_stream_cursors WHERE remote_authority_id=?1",
                [remote_authority_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))
    }

    fn put_hub_stream_cursor(
        &self,
        remote_authority_id: &str,
        sequence: i64,
        cursor: &str,
    ) -> Result<()> {
        if sequence < 1 || cursor.is_empty() || cursor.len() > 1024 {
            return Err(CoreError::new("HUB_CURSOR_INVALID", "invalid event cursor"));
        }
        self.connect()?
            .execute(
                "INSERT INTO hub_stream_cursors(remote_authority_id,sequence,cursor,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(remote_authority_id) DO UPDATE SET sequence=excluded.sequence,cursor=excluded.cursor,updated_at=excluded.updated_at WHERE excluded.sequence>hub_stream_cursors.sequence",
                params![remote_authority_id, sequence, cursor, now_rfc3339()?],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    fn clear_hub_stream_cursor(&self, remote_authority_id: &str) -> Result<()> {
        self.connect()?
            .execute(
                "DELETE FROM hub_stream_cursors WHERE remote_authority_id=?1",
                [remote_authority_id],
            )
            .map_err(|error| io_error("LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    /// Persists a resolved logical build.
    pub fn put_build(
        &self,
        id: BuildId,
        logical_hash: ContentHash,
        input: &LogicalBuildInput,
        status: &str,
        created_at: &str,
    ) -> Result<()> {
        let bytes = canonical_json(input)
            .map_err(|error| CoreError::new("BUILD_SERIALIZE_FAILED", error.to_string()))?;
        self.connect()?.execute("INSERT INTO builds(id,logical_hash,status,input_json,created_at) VALUES(?1,?2,?3,?4,?5)", params![id.to_string(), logical_hash.to_string(), status, bytes, created_at]).map_err(|error| io_error("BUILD_LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    /// Attaches an immutable bundle to a successful build.
    pub fn put_bundle(
        &self,
        build_id: BuildId,
        manifest: &ReleaseBundleManifest,
        created_at: &str,
    ) -> Result<ContentHash> {
        let hash = manifest
            .bundle_hash()
            .map_err(|error| CoreError::new("BUNDLE_HASH_FAILED", error.to_string()))?;
        let bytes = canonical_json(manifest)
            .map_err(|error| CoreError::new("BUNDLE_SERIALIZE_FAILED", error.to_string()))?;
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO bundles(hash,manifest_json,created_at) VALUES(?1,?2,?3)",
                params![hash.to_string(), bytes, created_at],
            )
            .map_err(|error| io_error("BUNDLE_LEDGER_WRITE_FAILED", error))?;
        let changed = transaction
            .execute(
                "UPDATE builds SET bundle_hash=?1,status='succeeded' WHERE id=?2",
                params![hash.to_string(), build_id.to_string()],
            )
            .map_err(|error| io_error("BUILD_LEDGER_WRITE_FAILED", error))?;
        if changed != 1 {
            return Err(CoreError::new("BUILD_NOT_FOUND", build_id.to_string()));
        }
        transaction
            .commit()
            .map_err(|error| io_error("LEDGER_TRANSACTION_FAILED", error))?;
        Ok(hash)
    }

    /// Loads a bundle manifest.
    pub fn bundle(&self, hash: ContentHash) -> Result<Option<ReleaseBundleManifest>> {
        self.read_json(
            "SELECT manifest_json FROM bundles WHERE hash=?1",
            &hash.to_string(),
        )
    }

    /// Creates a draft release with immutable request content.
    pub fn put_release(&self, release: &ReleaseRecord) -> Result<()> {
        let bytes = canonical_json(release)
            .map_err(|error| CoreError::new("RELEASE_SERIALIZE_FAILED", error.to_string()))?;
        self.connect()?.execute("INSERT INTO releases(id,bundle_hash,environment,status,request_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![release.release_id.to_string(), release.bundle_hash.to_string(), release.environment, status_text(release.status), bytes, release.created_at]).map_err(|error| io_error("RELEASE_LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    /// Loads a release.
    pub fn release(&self, id: ReleaseId) -> Result<Option<ReleaseRecord>> {
        self.read_json(
            "SELECT request_json FROM releases WHERE id=?1",
            &id.to_string(),
        )
    }

    /// Atomically advances a release state if its expected state still holds.
    pub fn transition_release(
        &self,
        id: ReleaseId,
        expected: ReleaseStatus,
        next: ReleaseStatus,
    ) -> Result<()> {
        expected
            .transition(next)
            .map_err(|error| CoreError::new("RELEASE_STATE_INVALID", error.to_string()))?;
        let connection = self.connect()?;
        let record: Vec<u8> = connection
            .query_row(
                "SELECT request_json FROM releases WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?
            .ok_or_else(|| CoreError::new("RELEASE_NOT_FOUND", id.to_string()))?;
        let mut release: ReleaseRecord = decode_json(&record)?;
        if release.status != expected {
            return Err(CoreError::new(
                "RELEASE_CONFLICT",
                format!("expected {expected:?}, found {:?}", release.status),
            ));
        }
        release.status = next;
        let bytes = canonical_json(&release)
            .map_err(|error| CoreError::new("RELEASE_SERIALIZE_FAILED", error.to_string()))?;
        connection
            .execute(
                "UPDATE releases SET status=?1,request_json=?2 WHERE id=?3 AND status=?4",
                params![
                    status_text(next),
                    bytes,
                    id.to_string(),
                    status_text(expected)
                ],
            )
            .map_err(|error| io_error("RELEASE_LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    /// Persists an append-only release receipt.
    pub fn put_receipt<T: Serialize>(
        &self,
        release_id: ReleaseId,
        phase: &str,
        receipt_id: &str,
        receipt: &T,
        created_at: &str,
    ) -> Result<()> {
        let bytes = canonical_json(receipt)
            .map_err(|error| CoreError::new("RECEIPT_SERIALIZE_FAILED", error.to_string()))?;
        self.connect()?.execute("INSERT INTO release_receipts(receipt_id,release_id,phase,receipt_json,created_at) VALUES(?1,?2,?3,?4,?5)", params![receipt_id, release_id.to_string(), phase, bytes, created_at]).map_err(|error| io_error("RECEIPT_LEDGER_WRITE_FAILED", error))?;
        Ok(())
    }

    fn read_json<T: DeserializeOwned>(&self, sql: &str, key: &str) -> Result<Option<T>> {
        let bytes: Option<Vec<u8>> = self
            .connect()?
            .query_row(sql, [key], |row| row.get(0))
            .optional()
            .map_err(|error| io_error("LEDGER_READ_FAILED", error))?;
        bytes.map(|bytes| decode_json(&bytes)).transpose()
    }
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|error| CoreError::new("LEDGER_CORRUPT", error.to_string()))
}

fn status_text(status: ReleaseStatus) -> String {
    serde_json::to_string(&status)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

fn art_status_text(status: ArtRevisionStatus) -> String {
    serde_json::to_string(&status)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Atomic local BLAKE3 content-addressed object store.
#[derive(Clone, Debug)]
pub struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    /// Creates an object store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("objects/b3-256"))
            .map_err(|error| io_error("CAS_DIRECTORY_FAILED", error))?;
        fs::create_dir_all(root.join("tmp"))
            .map_err(|error| io_error("CAS_DIRECTORY_FAILED", error))?;
        Ok(Self { root })
    }

    fn object_path(&self, hash: ContentHash) -> PathBuf {
        let digest = hash.to_string();
        let digest = digest.strip_prefix("b3-256:").unwrap_or(&digest);
        self.root
            .join("objects")
            .join("b3-256")
            .join(&digest[..2])
            .join(&digest[2..])
    }

    /// Writes bytes to a temporary file, flushes them, and atomically renames the
    /// verified object into its immutable destination.
    pub fn put(&self, bytes: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::digest(bytes);
        let destination = self.object_path(hash);
        if destination.exists() {
            self.verify(hash)?;
            return Ok(hash);
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error("CAS_DIRECTORY_FAILED", error))?;
        }
        let temporary = self.root.join("tmp").join(format!(
            "{}.{}.partial",
            hash.to_string().replace(':', "_"),
            uuid::Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| io_error("CAS_WRITE_FAILED", error))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| io_error("CAS_WRITE_FAILED", error))?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => {}
            Err(_) if destination.exists() => {
                let _ = fs::remove_file(&temporary);
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(io_error("CAS_COMMIT_FAILED", error));
            }
        }
        self.verify(hash)?;
        Ok(hash)
    }

    /// Reads and verifies an immutable object.
    pub fn get(&self, hash: ContentHash) -> Result<Vec<u8>> {
        let bytes = fs::read(self.object_path(hash))
            .map_err(|error| io_error("CAS_OBJECT_MISSING", error))?;
        if ContentHash::digest(&bytes) != hash {
            return Err(CoreError::new("CAS_HASH_MISMATCH", hash.to_string()));
        }
        Ok(bytes)
    }

    /// Verifies an object in place.
    pub fn verify(&self, hash: ContentHash) -> Result<()> {
        self.get(hash).map(|_| ())
    }
}

/// Art-cell bytes supplied by the authenticated Studio bridge or an import tool.
pub struct CapturedCell {
    /// Stable cell identity.
    pub cell_id: CellId,
    /// Managed slot path.
    pub slot_path: String,
    /// Exact native RBXM bytes.
    pub bytes: Vec<u8>,
}

/// Durable receipt for native bytes verified by the loopback Studio bridge.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StudioTransferRecord {
    /// Authenticated Studio session that uploaded the bytes.
    pub session_id: String,
    /// Idempotent bridge transfer identity.
    pub transfer_id: String,
    /// Canonical CAS identity of the exact RBXM bytes.
    pub content_hash: ContentHash,
    /// Exact byte length.
    pub size_bytes: u64,
    /// RFC 3339 UTC ingestion time.
    pub created_at: String,
}

/// One authenticated Studio capture request after native transfer verification.
#[derive(Clone, Debug)]
pub struct StudioCaptureRequest {
    /// Exact source Studio session.
    pub session_id: String,
    /// Previously ingested transfer.
    pub transfer_id: String,
    /// Stable cell ID supplied by the managed root attribute.
    pub cell_id: CellId,
    /// Exact managed parent slot in escaped DataModel path syntax.
    pub slot_path: String,
    /// Cell-local accepted base reported by Studio, when known.
    pub base_revision: Option<ArtRevisionId>,
    /// Monotonic Studio mutation epoch represented by the snapshot.
    pub mutation_epoch: u64,
    /// Stable local author label.
    pub author: String,
    /// Human checkpoint message.
    pub message: String,
}

/// Native cell material needed to apply one newly accepted revision.
#[derive(Clone, Debug)]
pub struct StudioAcceptedCell {
    /// Stable cell identity.
    pub cell_id: CellId,
    /// Parent slot where the native root is installed.
    pub slot_path: String,
    /// Exact native content identity.
    pub content_hash: ContentHash,
    /// Exact native bytes.
    pub bytes: Vec<u8>,
}

/// Result of one idempotent Studio capture orchestration transaction.
#[derive(Clone, Debug)]
pub struct StudioCaptureOutcome {
    /// Immutable proposed or accepted revision.
    pub revision: ArtRevision,
    /// Changed cell payload; populated for real-time accepted fan-out.
    pub changed_cell: StudioAcceptedCell,
    /// Whether channel policy allowed local acceptance and head advancement.
    pub locally_accepted: bool,
}

/// One Hub-verified accepted delta ready for local durable import.
#[derive(Clone, Debug)]
pub struct HubAcceptedRevisionRequest {
    /// Stable digest binding the configured Hub origin and remote project.
    pub remote_authority_id: String,
    /// Durable Hub outbox event ID.
    pub event_id: String,
    /// Immutable Hub revision ID.
    pub remote_revision_id: String,
    /// State root that must be the current local accepted base.
    pub base_state_root: Option<ContentHash>,
    /// State root committed by the accepted Hub manifest.
    pub state_root: ContentHash,
    /// True only for an authenticated full-snapshot cursor recovery.
    pub replace_state: bool,
    /// Display-safe remote author label.
    pub author: String,
    /// Display-safe acceptance summary.
    pub message: String,
    /// Bounded changed native cells verified by the Hub adapter.
    pub changed_cells: Vec<StudioAcceptedCell>,
}

/// Durable local representation of an accepted Hub event.
#[derive(Clone, Debug)]
pub struct HubAcceptedRevisionOutcome {
    /// Local immutable accepted revision used by Studio base attributes.
    pub revision: ArtRevision,
    /// True when the remote revision receipt already existed.
    pub duplicate: bool,
}

/// Workspace-scoped adapter from bridge events to CAS and the immutable ledger.
#[derive(Clone, Debug)]
pub struct LocalStudioOrchestrator {
    manifest: ProjectManifest,
    project_id: ProjectId,
    channel: String,
    ledger: Ledger,
    store: ContentStore,
}

impl LocalStudioOrchestrator {
    /// Opens a validated project and its local durable state.
    pub fn open(workspace: &Path) -> Result<Self> {
        let (manifest, _) = ProjectManifest::load(workspace).map_err(|report| {
            CoreError::new(
                "STUDIO_PROJECT_INVALID",
                report
                    .errors
                    .into_iter()
                    .map(|issue| format!("{} {}: {}", issue.code, issue.path, issue.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
        let project_id = fs::read_to_string(workspace.join(".neuman/project-id"))
            .map_err(|error| io_error("PROJECT_ID_READ_FAILED", error))?;
        let project_id = ProjectId::from_str(project_id.trim())
            .map_err(|error| CoreError::new("PROJECT_ID_INVALID", error.to_string()))?;
        let channel = manifest
            .project
            .default_art_channel
            .clone()
            .ok_or_else(|| {
                CoreError::new(
                    "ART_CHANNEL_REQUIRED",
                    "configure project.defaultArtChannel before Studio capture",
                )
            })?;
        if !manifest.art_channels.contains_key(&channel) {
            return Err(CoreError::new("ART_CHANNEL_UNKNOWN", channel));
        }
        Ok(Self {
            manifest,
            project_id,
            channel,
            ledger: Ledger::open(workspace.join(".neuman/state.sqlite3"))?,
            store: ContentStore::open(workspace.join(".neuman/cas"))?,
        })
    }

    /// Copies a bridge-verified quarantine file into CAS and durably records its
    /// session/transfer identity before the bridge acknowledges verification.
    pub fn ingest_verified_transfer(
        &self,
        session_id: &str,
        transfer_id: &str,
        declared_hash: ContentHash,
        declared_size: u64,
        quarantine_path: &Path,
    ) -> Result<StudioTransferRecord> {
        if let Some(existing) = self.ledger.studio_transfer(session_id, transfer_id)? {
            return if existing.content_hash == declared_hash && existing.size_bytes == declared_size
            {
                Ok(existing)
            } else {
                Err(CoreError::new(
                    "STUDIO_TRANSFER_IMMUTABLE",
                    "the Studio transfer ID already names different content",
                ))
            };
        }
        let metadata = fs::metadata(quarantine_path)
            .map_err(|error| io_error("STUDIO_TRANSFER_MISSING", error))?;
        if metadata.len() != declared_size {
            return Err(CoreError::new(
                "STUDIO_TRANSFER_SIZE_MISMATCH",
                format!("expected {declared_size}, found {}", metadata.len()),
            ));
        }
        let bytes = fs::read(quarantine_path)
            .map_err(|error| io_error("STUDIO_TRANSFER_READ_FAILED", error))?;
        let actual_hash = ContentHash::digest(&bytes);
        if actual_hash != declared_hash {
            return Err(CoreError::new(
                "STUDIO_TRANSFER_HASH_MISMATCH",
                declared_hash.to_string(),
            ));
        }
        let cas_hash = self.store.put(&bytes)?;
        if cas_hash != declared_hash {
            return Err(CoreError::new(
                "STUDIO_TRANSFER_CAS_MISMATCH",
                declared_hash.to_string(),
            ));
        }
        let record = StudioTransferRecord {
            session_id: session_id.to_owned(),
            transfer_id: transfer_id.to_owned(),
            content_hash: cas_hash,
            size_bytes: declared_size,
            created_at: now_rfc3339()?,
        };
        self.ledger.put_studio_transfer(&record)?;
        // CAS plus the ledger receipt are now durable. A failed cleanup leaves a
        // harmless verified quarantine duplicate for startup housekeeping.
        let _ = fs::remove_file(quarantine_path);
        Ok(record)
    }

    /// Commits a capture idempotently, overlays it on the accepted base state,
    /// and atomically advances the local head only when policy permits.
    pub fn commit_capture(&self, request: StudioCaptureRequest) -> Result<StudioCaptureOutcome> {
        if let Some(existing) = self
            .ledger
            .studio_capture_revision(&request.session_id, &request.transfer_id)?
        {
            let cell = existing.cells.get(&request.cell_id).ok_or_else(|| {
                CoreError::new(
                    "LEDGER_CORRUPT",
                    "capture receipt revision does not contain its cell",
                )
            })?;
            let bytes = self.store.get(cell.snapshot_hash)?;
            return Ok(StudioCaptureOutcome {
                locally_accepted: existing.status == ArtRevisionStatus::Accepted,
                changed_cell: StudioAcceptedCell {
                    cell_id: request.cell_id,
                    slot_path: cell.slot_path.clone(),
                    content_hash: cell.snapshot_hash,
                    bytes,
                },
                revision: existing,
            });
        }
        validate_bridge_token(&request.session_id, "ses_")?;
        validate_bridge_token(&request.transfer_id, "op_")?;
        if request.author.trim().is_empty() || request.author.len() > 200 {
            return Err(CoreError::new(
                "ART_AUTHOR_INVALID",
                "invalid Studio author",
            ));
        }
        if request.message.trim().is_empty() || request.message.len() > 500 {
            return Err(CoreError::new(
                "ART_MESSAGE_INVALID",
                "checkpoint message must contain 1..500 bytes",
            ));
        }
        self.validate_studio_slot(&request.slot_path)?;
        let transfer = self
            .ledger
            .studio_transfer(&request.session_id, &request.transfer_id)?
            .ok_or_else(|| CoreError::new("STUDIO_TRANSFER_NOT_FOUND", &request.transfer_id))?;
        let current_head = self
            .ledger
            .accepted_art_head(self.project_id, &self.channel)?;
        let requested_base = request
            .base_revision
            .map(|id| {
                self.ledger
                    .art_revision(id)?
                    .ok_or_else(|| CoreError::new("ART_BASE_NOT_FOUND", id.to_string()))
            })
            .transpose()?;
        if let Some(base) = &requested_base
            && (base.project_id != self.project_id
                || base.channel_id != self.channel
                || base.status != ArtRevisionStatus::Accepted)
        {
            return Err(CoreError::new(
                "ART_BASE_INVALID",
                "Studio base is not an accepted revision in the active project/channel",
            ));
        }
        let channel = self
            .manifest
            .art_channels
            .get(&self.channel)
            .expect("validated channel");
        let policy_approvals = self
            .manifest
            .policies
            .get(&channel.acceptance_policy)
            .and_then(|policy| policy.get("approvals"))
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let locally_accepted =
            !channel.protected && (channel.allow_offline_acceptance || policy_approvals == 0);
        let current_head_id = current_head
            .as_ref()
            .map(|revision| revision.art_revision_id);
        if current_head_id.is_some() && request.base_revision.is_none() {
            return Err(CoreError::new(
                "ART_BASE_REQUIRED",
                "Studio must name the accepted art head it edited; refresh context before capture",
            ));
        }
        if request.base_revision.is_some() && request.base_revision != current_head_id {
            return Err(CoreError::new(
                "ART_BASE_STALE",
                "Studio cell is based on an older accepted art head; rebase before acceptance",
            ));
        }
        let base = requested_base.as_ref().or(current_head.as_ref());
        let mut state = base
            .map(|revision| revision.cells.clone())
            .unwrap_or_default();
        state.insert(
            request.cell_id,
            ArtCellState {
                cell_id: request.cell_id,
                snapshot_hash: transfer.content_hash,
                slot_path: request.slot_path.clone(),
            },
        );
        let state_root_hash = ArtRevision::compute_state_root(&state)
            .map_err(|error| CoreError::new("ART_STATE_HASH_FAILED", error.to_string()))?;
        let revision = ArtRevision {
            art_revision_id: ArtRevisionId::new(),
            project_id: self.project_id,
            channel_id: self.channel.clone(),
            parents: base
                .map(|revision| vec![revision.art_revision_id])
                .unwrap_or_default(),
            cells: state,
            state_root_hash,
            author: request.author,
            message: request.message,
            created_at: now_rfc3339()?,
            status: if locally_accepted {
                ArtRevisionStatus::Accepted
            } else {
                ArtRevisionStatus::Proposed
            },
        };
        self.ledger.commit_studio_capture(
            &revision,
            &request.session_id,
            &request.transfer_id,
            request.mutation_epoch,
            current_head_id,
        )?;
        let bytes = self.store.get(transfer.content_hash)?;
        Ok(StudioCaptureOutcome {
            revision,
            changed_cell: StudioAcceptedCell {
                cell_id: request.cell_id,
                slot_path: request.slot_path,
                content_hash: transfer.content_hash,
                bytes,
            },
            locally_accepted,
        })
    }

    /// Current accepted head used when refreshing the bridge session context.
    pub fn accepted_head(&self) -> Result<Option<ArtRevision>> {
        self.ledger
            .accepted_art_head(self.project_id, &self.channel)
    }

    /// Materializes every immutable native cell in a revision from local CAS.
    pub fn materialize_revision_cells(
        &self,
        revision: &ArtRevision,
    ) -> Result<Vec<StudioAcceptedCell>> {
        if revision.project_id != self.project_id || revision.channel_id != self.channel {
            return Err(CoreError::new(
                "ART_REVISION_CONTEXT_MISMATCH",
                "revision is outside the active local project/channel",
            ));
        }
        revision
            .cells
            .values()
            .map(|cell| {
                Ok(StudioAcceptedCell {
                    cell_id: cell.cell_id,
                    slot_path: cell.slot_path.clone(),
                    content_hash: cell.snapshot_hash,
                    bytes: self.store.get(cell.snapshot_hash)?,
                })
            })
            .collect()
    }

    /// Atomically overlays a hash-verified Hub delta on the exact accepted base,
    /// commits its cells to local CAS, and advances the local accepted head.
    pub fn import_hub_accepted_revision(
        &self,
        request: HubAcceptedRevisionRequest,
    ) -> Result<HubAcceptedRevisionOutcome> {
        validate_remote_label(&request.remote_authority_id, "HUB_AUTHORITY_INVALID")?;
        validate_remote_label(&request.event_id, "HUB_EVENT_INVALID")?;
        validate_remote_label(&request.remote_revision_id, "HUB_REVISION_INVALID")?;
        if request.changed_cells.is_empty() || request.changed_cells.len() > 128 {
            return Err(CoreError::new(
                "HUB_REVISION_INVALID",
                "accepted Hub delta must contain 1..128 cells",
            ));
        }
        if request.author.trim().is_empty()
            || request.author.len() > 200
            || request.message.trim().is_empty()
            || request.message.len() > 500
        {
            return Err(CoreError::new(
                "HUB_REVISION_INVALID",
                "remote author or summary is invalid",
            ));
        }
        if let Some(existing_id) = self
            .ledger
            .hub_revision_local_id(&request.remote_authority_id, &request.remote_revision_id)?
        {
            let revision = self.ledger.art_revision(existing_id)?.ok_or_else(|| {
                CoreError::new("LEDGER_CORRUPT", "Hub receipt revision is missing")
            })?;
            if revision.state_root_hash != request.state_root {
                return Err(CoreError::new(
                    "HUB_REVISION_IMMUTABLE",
                    "the Hub revision ID was replayed with a different state root",
                ));
            }
            return Ok(HubAcceptedRevisionOutcome {
                revision,
                duplicate: true,
            });
        }

        let current = self
            .ledger
            .accepted_art_head(self.project_id, &self.channel)?;
        let current_root = current.as_ref().map(|revision| revision.state_root_hash);
        if !request.replace_state && current_root != request.base_state_root {
            return Err(CoreError::new(
                "HUB_BASE_STALE",
                format!(
                    "accepted Hub delta expects {:?}, local head is {:?}",
                    request.base_state_root, current_root
                ),
            ));
        }
        let mut state = if request.replace_state {
            BTreeMap::new()
        } else {
            current
                .as_ref()
                .map(|revision| revision.cells.clone())
                .unwrap_or_default()
        };
        let mut seen = BTreeSet::new();
        for cell in &request.changed_cells {
            if !seen.insert(cell.cell_id) {
                return Err(CoreError::new(
                    "HUB_REVISION_INVALID",
                    "accepted Hub delta repeats a cell ID",
                ));
            }
            self.validate_studio_slot(&cell.slot_path)?;
            if ContentHash::digest(&cell.bytes) != cell.content_hash {
                return Err(CoreError::new(
                    "HUB_CELL_HASH_MISMATCH",
                    cell.cell_id.to_string(),
                ));
            }
            let stored = self.store.put(&cell.bytes)?;
            if stored != cell.content_hash {
                return Err(CoreError::new(
                    "HUB_CELL_CAS_MISMATCH",
                    cell.cell_id.to_string(),
                ));
            }
            state.insert(
                cell.cell_id,
                ArtCellState {
                    cell_id: cell.cell_id,
                    snapshot_hash: cell.content_hash,
                    slot_path: cell.slot_path.clone(),
                },
            );
        }
        let computed = ArtRevision::compute_state_root(&state)
            .map_err(|error| CoreError::new("ART_STATE_HASH_FAILED", error.to_string()))?;
        if computed != request.state_root {
            return Err(CoreError::new(
                "HUB_STATE_ROOT_MISMATCH",
                format!("expected {}, computed {}", request.state_root, computed),
            ));
        }
        let revision = ArtRevision {
            art_revision_id: ArtRevisionId::new(),
            project_id: self.project_id,
            channel_id: self.channel.clone(),
            parents: current
                .as_ref()
                .map(|revision| vec![revision.art_revision_id])
                .unwrap_or_default(),
            cells: state,
            state_root_hash: computed,
            author: request.author,
            message: request.message,
            created_at: now_rfc3339()?,
            status: ArtRevisionStatus::Accepted,
        };
        let committed_id = self.ledger.commit_hub_revision(
            &revision,
            &request.remote_authority_id,
            &request.remote_revision_id,
            &request.event_id,
            current.as_ref().map(|revision| revision.art_revision_id),
        )?;
        let committed = self
            .ledger
            .art_revision(committed_id)?
            .ok_or_else(|| CoreError::new("LEDGER_CORRUPT", "committed Hub revision is missing"))?;
        Ok(HubAcceptedRevisionOutcome {
            duplicate: committed.art_revision_id != revision.art_revision_id,
            revision: committed,
        })
    }

    /// Loads the durable cursor used to resume this self-hosted Hub stream.
    pub fn hub_stream_cursor(&self, remote_authority_id: &str) -> Result<Option<(i64, String)>> {
        validate_remote_label(remote_authority_id, "HUB_AUTHORITY_INVALID")?;
        self.ledger.hub_stream_cursor(remote_authority_id)
    }

    /// Advances the durable Hub cursor only after an event was fully handled.
    pub fn put_hub_stream_cursor(
        &self,
        remote_authority_id: &str,
        sequence: i64,
        cursor: &str,
    ) -> Result<()> {
        validate_remote_label(remote_authority_id, "HUB_AUTHORITY_INVALID")?;
        self.ledger
            .put_hub_stream_cursor(remote_authority_id, sequence, cursor)
    }

    /// Clears an expired cursor before a bounded full-snapshot event replay.
    pub fn clear_hub_stream_cursor(&self, remote_authority_id: &str) -> Result<()> {
        validate_remote_label(remote_authority_id, "HUB_AUTHORITY_INVALID")?;
        self.ledger.clear_hub_stream_cursor(remote_authority_id)
    }

    fn validate_studio_slot(&self, slot_path: &str) -> Result<()> {
        let slot = normalize_data_model_path(slot_path)
            .ok_or_else(|| CoreError::new("ART_SLOT_INVALID", slot_path))?;
        let channel = self
            .manifest
            .art_channels
            .get(&self.channel)
            .expect("validated channel");
        let place = self
            .manifest
            .places
            .get(&channel.authoring_place)
            .ok_or_else(|| {
                CoreError::new("ART_AUTHORING_PLACE_UNKNOWN", &channel.authoring_place)
            })?;
        let allowed = place.ownership.iter().any(|root| {
            if root.owner != "studio-art" {
                return false;
            }
            let configured_channel = root.options.get("channel").and_then(Value::as_str);
            if configured_channel.is_some_and(|configured| configured != self.channel) {
                return false;
            }
            normalize_data_model_path(&root.path).is_some_and(|root_path| {
                slot == root_path
                    || slot
                        .strip_prefix(&root_path)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
        });
        if !allowed {
            return Err(CoreError::new(
                "ART_OWNERSHIP_VIOLATION",
                format!("{slot_path} is outside the active channel's Studio-art roots"),
            ));
        }
        Ok(())
    }
}

fn validate_remote_label(value: &str, code: &'static str) -> Result<()> {
    if value.len() < 4
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        return Err(CoreError::new(code, "invalid bounded remote identifier"));
    }
    Ok(())
}

fn validate_bridge_token(value: &str, prefix: &str) -> Result<()> {
    if value.len() < prefix.len() + 4
        || value.len() > 128
        || !value.starts_with(prefix)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(CoreError::new(
            "STUDIO_ID_INVALID",
            format!("invalid {prefix} identifier"),
        ));
    }
    Ok(())
}

fn now_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| CoreError::new("CLOCK_FORMAT_FAILED", error.to_string()))
}

/// Creates immutable art revision metadata and stores native cell bytes in CAS.
pub fn capture_art_revision(
    store: &ContentStore,
    ledger: &Ledger,
    project_id: ProjectId,
    channel: String,
    parents: Vec<ArtRevisionId>,
    cells: Vec<CapturedCell>,
    author: String,
    message: String,
    created_at: String,
    accepted: bool,
) -> Result<ArtRevision> {
    if cells.is_empty() {
        return Err(CoreError::new(
            "ART_CAPTURE_EMPTY",
            "at least one cell is required",
        ));
    }
    let mut state = BTreeMap::new();
    for cell in cells {
        if normalize_data_model_path(&cell.slot_path).is_none() {
            return Err(CoreError::new("ART_SLOT_INVALID", cell.slot_path));
        }
        let bytes_hash = store.put(&cell.bytes)?;
        let snapshot_hash = bytes_hash;
        if state
            .insert(
                cell.cell_id,
                ArtCellState {
                    cell_id: cell.cell_id,
                    snapshot_hash,
                    slot_path: cell.slot_path,
                },
            )
            .is_some()
        {
            return Err(CoreError::new(
                "ART_CELL_DUPLICATE",
                cell.cell_id.to_string(),
            ));
        }
    }
    let root = ArtRevision::compute_state_root(&state)
        .map_err(|error| CoreError::new("ART_STATE_HASH_FAILED", error.to_string()))?;
    let revision = ArtRevision {
        art_revision_id: ArtRevisionId::new(),
        project_id,
        channel_id: channel,
        parents,
        cells: state,
        state_root_hash: root,
        author,
        message,
        created_at,
        status: if accepted {
            ArtRevisionStatus::Accepted
        } else {
            ArtRevisionStatus::Proposed
        },
    };
    ledger.put_art_revision(&revision)?;
    Ok(revision)
}

/// Git workspace observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    /// Exact HEAD commit.
    pub head: GitOid,
    /// Current branch when attached.
    pub branch: Option<String>,
    /// Whether tracked or untracked files differ from HEAD.
    pub clean: bool,
    /// Porcelain-v2 status records, never file contents.
    pub changes: Vec<String>,
}

/// Uses the system Git executable with argument arrays and hooks disabled.
pub fn git_status(root: &Path, format: GitObjectFormat) -> Result<GitStatus> {
    let head_raw = run_git(root, &["rev-parse", "--verify", "HEAD"])?;
    let head = GitOid::parse_for(head_raw.trim(), format)
        .map_err(|error| CoreError::new("GIT_OID_INVALID", error.to_string()))?;
    let branch_raw = run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let status = run_git(root, &["status", "--porcelain=v2", "--untracked-files=all"])?;
    let changes: Vec<_> = status.lines().map(str::to_owned).collect();
    Ok(GitStatus {
        head,
        branch: branch_raw
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        clean: changes.is_empty(),
        changes,
    })
}

/// Proves that an exact object exists and is a commit.
pub fn validate_git_commit(root: &Path, oid: &GitOid) -> Result<()> {
    run_git(
        root,
        &["cat-file", "-e", &format!("{}^{{commit}}", oid.as_str())],
    )
    .map(|_| ())
}

fn run_git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .arg("-c")
        .arg("core.hooksPath=")
        .args(args)
        .output()
        .map_err(|error| io_error("GIT_EXEC_FAILED", error))?;
    if !output.status.success() {
        return Err(CoreError::new(
            "GIT_COMMAND_FAILED",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| CoreError::new("GIT_OUTPUT_INVALID", error.to_string()))
}

/// Resolves and persists an immutable build and its environment-neutral bundle.
pub fn create_build_bundle(
    manifest: &ProjectManifest,
    ledger: &Ledger,
    store: &ContentStore,
    input: LogicalBuildInput,
    candidate_place_bytes: &[u8],
    created_at: &str,
) -> Result<BuildResult> {
    if !manifest.places.contains_key(&input.place_key) {
        return Err(CoreError::new("BUILD_PLACE_UNKNOWN", &input.place_key));
    }
    let expected_manifest_hash = hash_canonical("neuman-project-manifest-v1\0", manifest)
        .map_err(|error| CoreError::new("BUILD_MANIFEST_HASH_FAILED", error.to_string()))?;
    if input.manifest_hash != expected_manifest_hash {
        return Err(CoreError::new(
            "BUILD_MANIFEST_MISMATCH",
            "resolved manifest does not match immutable build input",
        ));
    }
    let art = ledger
        .art_revision(input.art_revision_id)?
        .ok_or_else(|| CoreError::new("BUILD_ART_NOT_FOUND", input.art_revision_id.to_string()))?;
    if art.status != ArtRevisionStatus::Accepted {
        return Err(CoreError::new(
            "BUILD_ART_NOT_ACCEPTED",
            input.art_revision_id.to_string(),
        ));
    }
    if art.state_root_hash != input.art_state_root_hash {
        return Err(CoreError::new(
            "BUILD_ART_ROOT_MISMATCH",
            "art revision state root changed or is corrupt",
        ));
    }
    let logical_hash = input
        .logical_hash()
        .map_err(|error| CoreError::new("BUILD_HASH_FAILED", error.to_string()))?;
    let build_id = BuildId::new();
    ledger.put_build(build_id, logical_hash, &input, "assembling", created_at)?;
    let artifact_hash = store.put(candidate_place_bytes)?;
    let bundle = ReleaseBundleManifest {
        schema_version: "1.0".into(),
        logical_build_hash: logical_hash,
        place_key: input.place_key.clone(),
        artifacts: vec![BundleArtifact {
            name: "place-candidate".into(),
            content_hash: artifact_hash,
            size_bytes: candidate_place_bytes.len() as u64,
            media_type: "application/x-roblox-rbxl".into(),
        }],
        reproducibility: Reproducibility::Input,
    };
    let bundle_hash = ledger.put_bundle(build_id, &bundle, created_at)?;
    Ok(BuildResult {
        build_id,
        logical_build_hash: logical_hash,
        bundle_hash,
        bundle,
    })
}

/// Successful build identities.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    /// Build entity ID.
    pub build_id: BuildId,
    /// Canonical input identity.
    pub logical_build_hash: ContentHash,
    /// Immutable bundle identity.
    pub bundle_hash: ContentHash,
    /// Environment-neutral manifest.
    pub bundle: ReleaseBundleManifest,
}

/// OAuth PKCE authorization request secrets. Store verifier/state only in protected
/// local memory/keychain until the redirect completes.
#[derive(Clone)]
pub struct PkceRequest {
    /// Browser URL.
    pub authorization_url: Url,
    /// High-entropy verifier, intentionally omitted from `Debug` output.
    verifier: String,
    /// CSRF state.
    state: String,
    redirect_uri: Url,
}

impl std::fmt::Debug for PkceRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PkceRequest")
            .field(
                "authorization_origin",
                &self.authorization_url.origin().ascii_serialization(),
            )
            .field("redirect_uri", &self.redirect_uri)
            .field("verifier", &"[REDACTED]")
            .field("state", &"[REDACTED]")
            .finish()
    }
}

impl PkceRequest {
    /// Returns the verifier to the token exchanger without serializing it.
    #[must_use]
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// Validates exact redirect URI, state, and a single authorization code.
    pub fn consume_redirect(&self, redirect: &Url) -> Result<String> {
        if redirect.scheme() != self.redirect_uri.scheme()
            || redirect.host_str() != self.redirect_uri.host_str()
            || redirect.port_or_known_default() != self.redirect_uri.port_or_known_default()
            || redirect.path() != self.redirect_uri.path()
        {
            return Err(CoreError::new(
                "OAUTH_REDIRECT_MISMATCH",
                "redirect origin/path does not match the registered loopback callback",
            ));
        }
        let query: BTreeMap<_, _> = redirect.query_pairs().into_owned().collect();
        if query.get("state") != Some(&self.state) {
            return Err(CoreError::new(
                "OAUTH_STATE_MISMATCH",
                "OAuth state did not match",
            ));
        }
        if let Some(error) = query.get("error") {
            return Err(CoreError::new("OAUTH_PROVIDER_ERROR", error.clone()));
        }
        query
            .get("code")
            .filter(|code| !code.is_empty())
            .cloned()
            .ok_or_else(|| {
                CoreError::new(
                    "OAUTH_CODE_MISSING",
                    "redirect did not contain an authorization code",
                )
            })
    }
}

/// Creates a Roblox first-party OAuth authorization-code request with S256 PKCE.
pub fn roblox_pkce_authorization(
    client_id: &str,
    redirect_uri: Url,
    scopes: &[String],
) -> Result<PkceRequest> {
    if client_id.is_empty() || scopes.is_empty() {
        return Err(CoreError::new(
            "OAUTH_REQUEST_INVALID",
            "client ID and at least one scope are required",
        ));
    }
    if redirect_uri.scheme() != "http"
        || !matches!(
            redirect_uri.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
    {
        return Err(CoreError::new(
            "OAUTH_REDIRECT_UNSAFE",
            "public-client redirect must be an HTTP loopback address",
        ));
    }
    let mut verifier_bytes = [0_u8; 32];
    let mut state_bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    rand::rng().fill_bytes(&mut state_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let state = URL_SAFE_NO_PAD.encode(state_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut authorization_url = Url::parse("https://apis.roblox.com/oauth/v1/authorize")
        .map_err(|error| CoreError::new("OAUTH_ENDPOINT_INVALID", error.to_string()))?;
    authorization_url
        .query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri.as_str())
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(PkceRequest {
        authorization_url,
        verifier,
        state,
        redirect_uri,
    })
}

/// Operator-controlled secret for the API-key-only Place Publishing endpoint.
pub struct OperatorApiKey(String);

impl std::fmt::Debug for OperatorApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperatorApiKey([REDACTED])")
    }
}

impl OperatorApiKey {
    /// Accepts only a raw operator secret. Cookie strings and header injection are rejected.
    pub fn parse(value: String) -> Result<Self> {
        let lower = value.to_ascii_lowercase();
        if value.len() < 16
            || value.len() > 4096
            || value.chars().any(char::is_whitespace)
            || lower.contains(".roblosecurity")
            || lower.contains("cookie:")
            || value.contains('\r')
            || value.contains('\n')
        {
            return Err(CoreError::new(
                "ROBLOX_OPERATOR_KEY_INVALID",
                "operator key is malformed or resembles a forbidden session cookie",
            ));
        }
        Ok(Self(value))
    }
}

/// Guarded client for Roblox's documented API-key-only Place Publishing endpoint.
pub struct RobloxPlacePublisher {
    client: Client,
    base_url: Url,
    key: OperatorApiKey,
}

impl RobloxPlacePublisher {
    /// Creates a production publisher pinned to the official HTTPS origin.
    pub fn new(key: OperatorApiKey) -> Result<Self> {
        Self::with_base_url(
            key,
            Url::parse("https://apis.roblox.com/").expect("constant official URL"),
            false,
        )
    }

    /// Creates a publisher for a loopback mock. This rejects every non-loopback HTTP origin.
    #[doc(hidden)]
    pub fn new_for_loopback_test(key: OperatorApiKey, base_url: Url) -> Result<Self> {
        Self::with_base_url(key, base_url, true)
    }

    fn with_base_url(key: OperatorApiKey, base_url: Url, allow_loopback: bool) -> Result<Self> {
        let official =
            base_url.scheme() == "https" && base_url.host_str() == Some("apis.roblox.com");
        let loopback = allow_loopback
            && base_url.scheme() == "http"
            && matches!(
                base_url.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            );
        if !official && !loopback {
            return Err(CoreError::new(
                "ROBLOX_ORIGIN_FORBIDDEN",
                "publisher origin is not an allowlisted official/loopback-test origin",
            ));
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .map_err(|error| CoreError::new("ROBLOX_CLIENT_FAILED", error.to_string()))?;
        Ok(Self {
            client,
            base_url,
            key,
        })
    }

    /// Publishes an RBXL artifact and returns provider version evidence.
    pub async fn publish(
        &self,
        universe_id: &RobloxId,
        place_id: &RobloxId,
        bytes: &[u8],
    ) -> Result<PublishReceipt> {
        if bytes.is_empty() || bytes.len() > 104_857_600 {
            return Err(CoreError::new(
                "ROBLOX_PLACE_SIZE_INVALID",
                "place artifact must be 1..104857600 bytes",
            ));
        }
        let endpoint = self
            .base_url
            .join(&format!(
                "universes/v1/{universe_id}/places/{place_id}/versions?versionType=Published"
            ))
            .map_err(|error| CoreError::new("ROBLOX_URL_FAILED", error.to_string()))?;
        let response = self
            .client
            .post(endpoint)
            .header("x-api-key", &self.key.0)
            .header("content-type", "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|error| {
                CoreError::retryable("ROBLOX_PUBLISH_UNAVAILABLE", error.to_string())
            })?;
        let status = response.status();
        let request_id = response
            .headers()
            .get("roblox-id")
            .or_else(|| response.headers().get("x-request-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .bytes()
            .await
            .map_err(|error| CoreError::retryable("ROBLOX_RESPONSE_FAILED", error.to_string()))?;
        if !status.is_success() {
            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            let message = format!("provider returned HTTP {}", status.as_u16());
            return Err(if retryable {
                CoreError::retryable("ROBLOX_PUBLISH_REJECTED", message)
            } else {
                CoreError::new("ROBLOX_PUBLISH_REJECTED", message)
            });
        }
        #[derive(Deserialize)]
        struct Response {
            #[serde(rename = "versionNumber", alias = "version_number")]
            version_number: u64,
        }
        let result: Response = serde_json::from_slice(&body)
            .map_err(|error| CoreError::new("ROBLOX_RESPONSE_INVALID", error.to_string()))?;
        Ok(PublishReceipt {
            universe_id: universe_id.clone(),
            place_id: place_id.clone(),
            version_number: result.version_number,
            request_id,
            artifact_hash: ContentHash::digest(bytes),
            commit_state: PublishCommitState::Committed,
        })
    }
}

/// Provider commit-point classification.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PublishCommitState {
    /// Version was returned by Roblox.
    Committed,
    /// Request outcome requires reconciliation.
    Unknown,
}

/// Evidence returned after the external publish commit point.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReceipt {
    /// Exact universe target.
    pub universe_id: RobloxId,
    /// Exact place target.
    pub place_id: RobloxId,
    /// Observed new place version.
    pub version_number: u64,
    /// Provider request/correlation ID.
    pub request_id: Option<String>,
    /// Artifact sent.
    pub artifact_hash: ContentHash,
    /// External commit certainty.
    pub commit_state: PublishCommitState,
}

/// Immutable release request stored in the ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRecord {
    /// Release ID.
    pub release_id: ReleaseId,
    /// Project ID.
    pub project_id: ProjectId,
    /// Exact immutable bundle.
    pub bundle_hash: ContentHash,
    /// Environment key.
    pub environment: String,
    /// Place key.
    pub place_key: String,
    /// Exact target.
    pub target: RobloxTarget,
    /// Current state.
    pub status: ReleaseStatus,
    /// Requesting actor.
    pub requested_by: String,
    /// RFC 3339 timestamp.
    pub created_at: String,
}

/// All fail-closed evidence required immediately before one place mutation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightEvidence {
    /// Release is approved and still active.
    pub approved: bool,
    /// Every bundle object is local and hash-verified.
    pub bundle_verified: bool,
    /// Actor/key can mutate exactly this target.
    pub permission_verified: bool,
    /// Expected predecessor matches the observed version/deployment.
    pub predecessor_matches: bool,
    /// Drift state: `clean`, `version-drift`, `content-drift`, or `unknown`.
    pub drift_status: String,
    /// Whether an unexpired per-target lease is held.
    pub lease_held: bool,
    /// Staging proof for the exact bundle exists when policy requires it.
    pub staging_proof_valid: bool,
}

/// Immutable preflight decision/receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReceipt {
    /// Deterministic evidence identity.
    pub receipt_hash: ContentHash,
    /// Release being checked.
    pub release_id: ReleaseId,
    /// Whether mutation may begin.
    pub passed: bool,
    /// Stable failed gates.
    pub failed_gates: Vec<String>,
    /// Exact evidence.
    pub evidence: PreflightEvidence,
    /// RFC 3339 observation time.
    pub observed_at: String,
}

/// Evaluates release gates without mutating provider state.
pub fn release_preflight(
    release: &ReleaseRecord,
    evidence: PreflightEvidence,
    observed_at: String,
) -> Result<PreflightReceipt> {
    if release.status != ReleaseStatus::Approved && release.status != ReleaseStatus::Preflighting {
        return Err(CoreError::new(
            "RELEASE_NOT_APPROVED",
            "release must be approved before preflight",
        ));
    }
    let mut failed = Vec::new();
    if !evidence.approved {
        failed.push("approval".into());
    }
    if !evidence.bundle_verified {
        failed.push("bundle-integrity".into());
    }
    if !evidence.permission_verified {
        failed.push("target-permission".into());
    }
    if !evidence.predecessor_matches {
        failed.push("predecessor".into());
    }
    if evidence.drift_status != "clean" {
        failed.push("drift".into());
    }
    if !evidence.lease_held {
        failed.push("release-lease".into());
    }
    if !evidence.staging_proof_valid {
        failed.push("staging-proof".into());
    }
    let receipt_hash = hash_canonical("neuman-release-preflight-v1\0", &serde_json::json!({"releaseId":release.release_id,"bundleHash":release.bundle_hash,"target":release.target,"evidence":evidence,"observedAt":observed_at})).map_err(|error| CoreError::new("PREFLIGHT_HASH_FAILED", error.to_string()))?;
    Ok(PreflightReceipt {
        receipt_hash,
        release_id: release.release_id,
        passed: failed.is_empty(),
        failed_gates: failed,
        evidence,
        observed_at,
    })
}

/// A safe starter manifest. Production target IDs are deliberately absent.
pub fn starter_manifest(slug: &str, display_name: &str) -> Result<String> {
    if !valid_slug(slug) {
        return Err(CoreError::new("PROJECT_SLUG_INVALID", slug));
    }
    if display_name.is_empty() || display_name.chars().count() > 100 {
        return Err(CoreError::new("PROJECT_NAME_INVALID", display_name));
    }
    Ok(format!(
        r#"schemaVersion: "1.0"
project:
  slug: {slug}
  displayName: {display_name:?}
  defaultPlace: lobby
  defaultArtChannel: art-main
repository:
  provider: local
  defaultBranch: main
  objectFormat: sha1
  projectFile: default.project.json
  requireCleanWorktreeForBuild: true
  allowSubmodules: false
  allowGitHooks: false
toolchain:
  neuman: ">=0.1.0 <0.2.0"
  rojo: {{ version: "7.7.0", source: bundled }}
providers:
  artStore: {{ type: local-cas, options: {{}} }}
artChannels:
  art-main:
    displayName: Main Art
    protected: false
    authoringPlace: lobby
    acceptancePolicy: art-review
    lockPolicy: advisory
    allowOfflineCapture: true
    allowOfflineAcceptance: false
environments:
  authoring: {{ kind: authoring, productionImpact: false }}
places:
  lobby:
    displayName: Lobby
    baseTemplate: {{ type: repository-file, path: places/lobby.base.rbxl }}
    ownership:
      - {{ id: server-code, path: /ServerScriptService, owner: git-code, projectPath: src/server, unknownInstances: reject }}
      - {{ id: world-art, path: /Workspace/Art, owner: studio-art, channel: art-main, unknownInstances: reject }}
    validationProfile: default
    releasePolicy: standard
policies:
  art-review: {{ type: art-acceptance, approvals: 0 }}
  standard: {{ type: release, requireAcceptedArtRevision: true, requireNoUnknownDrift: true }}
validation: {{}}
extensions: {{}}
"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener, thread};

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("neuman-core-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn starter_manifest_validates_and_hashes() {
        let yaml = starter_manifest("test-game", "Test Game").unwrap();
        let (_, report) = ProjectManifest::parse(yaml.as_bytes()).unwrap();
        assert!(report.valid);
        assert!(report.manifest_hash.is_some());
    }

    #[test]
    fn overlapping_ownership_is_rejected() {
        let yaml = starter_manifest("test-game", "Test Game").unwrap().replace(
            "      - { id: world-art, path: /Workspace/Art, owner: studio-art, channel: art-main, unknownInstances: reject }",
            "      - { id: world, path: /Workspace, owner: studio-art, channel: art-main, unknownInstances: capture }\n      - { id: world-art, path: /Workspace/Art, owner: studio-art, channel: art-main, unknownInstances: reject }",
        );
        let report = ProjectManifest::parse(yaml.as_bytes()).unwrap_err();
        assert!(
            report
                .errors
                .iter()
                .any(|issue| issue.code == "OWNERSHIP_OVERLAP")
        );
    }

    #[test]
    fn provider_secrets_and_yaml_aliases_are_rejected() {
        let yaml = starter_manifest("test-game", "Test Game").unwrap().replace(
            "artStore: { type: local-cas, options: {} }",
            "artStore: { type: s3, apiKey: forbidden }",
        );
        assert!(
            ProjectManifest::parse(yaml.as_bytes())
                .unwrap_err()
                .errors
                .iter()
                .any(|issue| issue.code == "SECRET_IN_MANIFEST")
        );
        assert!(
            ProjectManifest::parse(b"schemaVersion: &v 1.0\nproject: *v")
                .unwrap_err()
                .errors
                .iter()
                .any(|issue| issue.code == "YAML_ALIAS_FORBIDDEN")
        );
    }

    #[test]
    fn cas_is_idempotent_and_detects_corruption() {
        let root = temp_dir();
        let store = ContentStore::open(root.join("cas")).unwrap();
        let hash = store.put(b"native rbxm").unwrap();
        assert_eq!(store.put(b"native rbxm").unwrap(), hash);
        assert_eq!(store.get(hash).unwrap(), b"native rbxm");
        fs::write(store.object_path(hash), b"corrupt").unwrap();
        assert_eq!(store.get(hash).unwrap_err().code, "CAS_HASH_MISMATCH");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn immutable_art_revision_and_build_bundle_round_trip() {
        let root = temp_dir();
        let ledger = Ledger::open(root.join("state.sqlite3")).unwrap();
        let store = ContentStore::open(root.join("cas")).unwrap();
        let project_id = ProjectId::new();
        let revision = capture_art_revision(
            &store,
            &ledger,
            project_id,
            "art-main".into(),
            vec![],
            vec![CapturedCell {
                cell_id: CellId::new(),
                slot_path: "/Workspace/Art/Tree".into(),
                bytes: b"rbxm".to_vec(),
            }],
            "artist".into(),
            "Tree".into(),
            "2026-07-09T00:00:00.000Z".into(),
            true,
        )
        .unwrap();
        assert_eq!(
            ledger
                .art_revision(revision.art_revision_id)
                .unwrap()
                .unwrap(),
            revision
        );
        let mut altered = revision.clone();
        altered.message = "rewrite".into();
        assert_eq!(
            ledger.put_art_revision(&altered).unwrap_err().code,
            "ART_REVISION_IMMUTABLE"
        );
        let manifest = ProjectManifest::parse(
            starter_manifest("test-game", "Test Game")
                .unwrap()
                .as_bytes(),
        )
        .unwrap()
        .0;
        let manifest_hash = hash_canonical("neuman-project-manifest-v1\0", &manifest).unwrap();
        let input = LogicalBuildInput {
            schema_version: "1.0".into(),
            project_id,
            place_key: "lobby".into(),
            repository: crate::domain::BuildRepositoryIdentity {
                id: "local:test".into(),
                object_format: GitObjectFormat::Sha1,
            },
            code_commit: GitOid::parse_for(&"a".repeat(40), GitObjectFormat::Sha1).unwrap(),
            art_revision_id: revision.art_revision_id,
            art_state_root_hash: revision.state_root_hash,
            base_template_hash: ContentHash::digest(b"base"),
            dependency_manifest_hash: ContentHash::digest(b"deps"),
            toolchain_lock_hash: ContentHash::digest(b"lock"),
            policy_revision_hash: ContentHash::digest(b"policy"),
            manifest_hash,
            profile: "release".into(),
        };
        let result = create_build_bundle(
            &manifest,
            &ledger,
            &store,
            input,
            b"rbxl candidate",
            "2026-07-09T01:00:00.000Z",
        )
        .unwrap();
        assert_eq!(
            ledger.bundle(result.bundle_hash).unwrap().unwrap(),
            result.bundle
        );
        let _ = fs::remove_dir_all(root);
    }

    fn studio_orchestrator(root: &Path, manifest_text: &str) -> LocalStudioOrchestrator {
        let manifest = ProjectManifest::parse(manifest_text.as_bytes()).unwrap().0;
        let project_id = ProjectId::new();
        fs::create_dir_all(root.join(".neuman")).unwrap();
        fs::write(root.join(".neuman/project-id"), project_id.to_string()).unwrap();
        fs::write(root.join("neuman.project.yaml"), manifest_text).unwrap();
        LocalStudioOrchestrator {
            manifest,
            project_id,
            channel: "art-main".into(),
            ledger: Ledger::open(root.join(".neuman/state.sqlite3")).unwrap(),
            store: ContentStore::open(root.join(".neuman/cas")).unwrap(),
        }
    }

    #[test]
    fn studio_capture_is_durable_idempotent_and_overlays_the_accepted_head() {
        let root = temp_dir();
        let manifest = starter_manifest("test-game", "Test Game").unwrap();
        let orchestrator = studio_orchestrator(&root, &manifest);
        let first_bytes = b"first native cell";
        let first_hash = ContentHash::digest(first_bytes);
        let first_path = root.join("first.verified");
        fs::write(&first_path, first_bytes).unwrap();
        orchestrator
            .ingest_verified_transfer(
                "ses_source1234",
                "op_transfer1234",
                first_hash,
                first_bytes.len() as u64,
                &first_path,
            )
            .unwrap();
        assert!(!first_path.exists());
        let first_cell = CellId::new();
        let request = StudioCaptureRequest {
            session_id: "ses_source1234".into(),
            transfer_id: "op_transfer1234".into(),
            cell_id: first_cell,
            slot_path: "/Workspace/Art".into(),
            base_revision: None,
            mutation_epoch: 7,
            author: "roblox:42".into(),
            message: "Studio checkpoint".into(),
        };
        let first = orchestrator.commit_capture(request.clone()).unwrap();
        assert!(first.locally_accepted);
        assert_eq!(first.revision.status, ArtRevisionStatus::Accepted);
        assert_eq!(first.revision.cells.len(), 1);
        let duplicate = orchestrator.commit_capture(request).unwrap();
        assert_eq!(
            duplicate.revision.art_revision_id,
            first.revision.art_revision_id
        );

        let second_bytes = b"second native cell";
        let second_hash = ContentHash::digest(second_bytes);
        let second_path = root.join("second.verified");
        fs::write(&second_path, second_bytes).unwrap();
        orchestrator
            .ingest_verified_transfer(
                "ses_source1234",
                "op_transfer5678",
                second_hash,
                second_bytes.len() as u64,
                &second_path,
            )
            .unwrap();
        let mut second_request = StudioCaptureRequest {
            session_id: "ses_source1234".into(),
            transfer_id: "op_transfer5678".into(),
            cell_id: CellId::new(),
            slot_path: "/Workspace/Art".into(),
            base_revision: None,
            mutation_epoch: 9,
            author: "roblox:42".into(),
            message: "Second checkpoint".into(),
        };
        assert_eq!(
            orchestrator
                .commit_capture(second_request.clone())
                .unwrap_err()
                .code,
            "ART_BASE_REQUIRED"
        );
        second_request.base_revision = Some(first.revision.art_revision_id);
        let second = orchestrator.commit_capture(second_request).unwrap();
        assert_eq!(
            second.revision.parents,
            vec![first.revision.art_revision_id]
        );
        assert_eq!(second.revision.cells.len(), 2);
        assert_eq!(
            orchestrator
                .accepted_head()
                .unwrap()
                .unwrap()
                .art_revision_id,
            second.revision.art_revision_id
        );

        let stale_bytes = b"stale native cell";
        let stale_hash = ContentHash::digest(stale_bytes);
        let stale_path = root.join("stale.verified");
        fs::write(&stale_path, stale_bytes).unwrap();
        orchestrator
            .ingest_verified_transfer(
                "ses_stale12345",
                "op_stale123456",
                stale_hash,
                stale_bytes.len() as u64,
                &stale_path,
            )
            .unwrap();
        assert_eq!(
            orchestrator
                .commit_capture(StudioCaptureRequest {
                    session_id: "ses_stale12345".into(),
                    transfer_id: "op_stale123456".into(),
                    cell_id: first_cell,
                    slot_path: "/Workspace/Art".into(),
                    base_revision: Some(first.revision.art_revision_id),
                    mutation_epoch: 10,
                    author: "roblox:99".into(),
                    message: "Stale checkpoint".into(),
                })
                .unwrap_err()
                .code,
            "ART_BASE_STALE"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn protected_studio_capture_remains_a_proposal_and_ownership_fails_closed() {
        let root = temp_dir();
        let manifest = starter_manifest("test-game", "Test Game")
            .unwrap()
            .replace("    protected: false", "    protected: true");
        let orchestrator = studio_orchestrator(&root, &manifest);
        let bytes = b"protected native cell";
        let hash = ContentHash::digest(bytes);
        let path = root.join("protected.verified");
        fs::write(&path, bytes).unwrap();
        orchestrator
            .ingest_verified_transfer(
                "ses_source1234",
                "op_protected1234",
                hash,
                bytes.len() as u64,
                &path,
            )
            .unwrap();
        let mut request = StudioCaptureRequest {
            session_id: "ses_source1234".into(),
            transfer_id: "op_protected1234".into(),
            cell_id: CellId::new(),
            slot_path: "/ServerScriptService".into(),
            base_revision: None,
            mutation_epoch: 1,
            author: "roblox:42".into(),
            message: "Protected checkpoint".into(),
        };
        assert_eq!(
            orchestrator
                .commit_capture(request.clone())
                .unwrap_err()
                .code,
            "ART_OWNERSHIP_VIOLATION"
        );
        request.slot_path = "/Workspace/Art".into();
        let outcome = orchestrator.commit_capture(request).unwrap();
        assert!(!outcome.locally_accepted);
        assert_eq!(outcome.revision.status, ArtRevisionStatus::Proposed);
        assert!(orchestrator.accepted_head().unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hub_revision_import_is_durable_idempotent_and_base_bound() {
        let root = temp_dir();
        let manifest = starter_manifest("test-game", "Test Game")
            .unwrap()
            .replace("    protected: false", "    protected: true");
        let orchestrator = studio_orchestrator(&root, &manifest);
        let bytes = b"remote accepted cell".to_vec();
        let cell_id = CellId::new();
        let content_hash = ContentHash::digest(&bytes);
        let mut state = BTreeMap::new();
        state.insert(
            cell_id,
            ArtCellState {
                cell_id,
                snapshot_hash: content_hash,
                slot_path: "/Workspace/Art".into(),
            },
        );
        let state_root = ArtRevision::compute_state_root(&state).unwrap();
        let request = HubAcceptedRevisionRequest {
            remote_authority_id: "hub_authority_1234".into(),
            event_id: "evt_remote_1234".into(),
            remote_revision_id: "arev_remote_1234".into(),
            base_state_root: None,
            state_root,
            replace_state: false,
            author: "Hub artist".into(),
            message: "Accepted remote checkpoint".into(),
            changed_cells: vec![StudioAcceptedCell {
                cell_id,
                slot_path: "/Workspace/Art".into(),
                content_hash,
                bytes,
            }],
        };
        let first = orchestrator
            .import_hub_accepted_revision(request.clone())
            .unwrap();
        assert!(!first.duplicate);
        assert_eq!(first.revision.state_root_hash, state_root);
        let duplicate = orchestrator.import_hub_accepted_revision(request).unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(
            duplicate.revision.art_revision_id,
            first.revision.art_revision_id
        );
        assert_eq!(
            orchestrator
                .accepted_head()
                .unwrap()
                .unwrap()
                .art_revision_id,
            first.revision.art_revision_id
        );
        orchestrator
            .put_hub_stream_cursor("hub_authority_1234", 8, "cursor-safe-value")
            .unwrap();
        orchestrator
            .put_hub_stream_cursor("hub_authority_1234", 7, "cursor-stale-value")
            .unwrap();
        assert_eq!(
            orchestrator
                .hub_stream_cursor("hub_authority_1234")
                .unwrap()
                .unwrap(),
            (8, "cursor-safe-value".into())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pkce_has_s256_and_rejects_state_confusion() {
        let redirect = Url::parse("http://127.0.0.1:34567/oauth/callback").unwrap();
        let request = roblox_pkce_authorization(
            "public-client",
            redirect.clone(),
            &["openid".into(), "profile".into()],
        )
        .unwrap();
        let query: BTreeMap<_, _> = request
            .authorization_url
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_ne!(query["code_challenge"], request.verifier());
        let debug = format!("{request:?}");
        assert!(!debug.contains(request.verifier()));
        assert!(!debug.contains(&query["state"]));
        let bad = Url::parse("http://127.0.0.1:34567/oauth/callback?code=x&state=wrong").unwrap();
        assert_eq!(
            request.consume_redirect(&bad).unwrap_err().code,
            "OAUTH_STATE_MISMATCH"
        );
        let good = redirect
            .join(&format!("?code=ok&state={}", query["state"]))
            .unwrap();
        assert_eq!(request.consume_redirect(&good).unwrap(), "ok");
    }

    #[test]
    fn preflight_fails_closed_for_unknown_drift() {
        let release = ReleaseRecord {
            release_id: ReleaseId::new(),
            project_id: ProjectId::new(),
            bundle_hash: ContentHash::digest(b"bundle"),
            environment: "production".into(),
            place_key: "lobby".into(),
            target: RobloxTarget {
                universe_id: "1".parse().unwrap(),
                place_id: "2".parse().unwrap(),
                creator: Creator {
                    kind: "group".into(),
                    id: "3".parse().unwrap(),
                },
            },
            status: ReleaseStatus::Approved,
            requested_by: "operator".into(),
            created_at: "2026-07-09T00:00:00.000Z".into(),
        };
        let receipt = release_preflight(
            &release,
            PreflightEvidence {
                approved: true,
                bundle_verified: true,
                permission_verified: true,
                predecessor_matches: true,
                drift_status: "unknown".into(),
                lease_held: true,
                staging_proof_valid: true,
            },
            "2026-07-09T00:01:00.000Z".into(),
        )
        .unwrap();
        assert!(!receipt.passed);
        assert!(receipt.failed_gates.contains(&"drift".to_string()));
    }

    #[tokio::test]
    async fn publisher_uses_api_key_header_and_parses_receipt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 8192];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request
                    .starts_with("POST /universes/v1/11/places/22/versions?versionType=Published")
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-api-key: this-is-an-operator-key")
            );
            let body = r#"{"versionNumber":42}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nroblox-id: req-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let key = OperatorApiKey::parse("this-is-an-operator-key".into()).unwrap();
        let publisher = RobloxPlacePublisher::new_for_loopback_test(
            key,
            Url::parse(&format!("http://{address}/")).unwrap(),
        )
        .unwrap();
        let receipt = publisher
            .publish(&"11".parse().unwrap(), &"22".parse().unwrap(), b"rbxl")
            .await
            .unwrap();
        assert_eq!(receipt.version_number, 42);
        assert_eq!(receipt.request_id.as_deref(), Some("req-1"));
        server.join().unwrap();
    }

    #[test]
    fn operator_key_rejects_cookie_material() {
        assert_eq!(
            OperatorApiKey::parse(".ROBLOSECURITY=not-allowed".into())
                .unwrap_err()
                .code,
            "ROBLOX_OPERATOR_KEY_INVALID"
        );
    }
}
