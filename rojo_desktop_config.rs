//! Desktop-safe adapter from a selected NeuMan workspace to a live Rojo request.
//!
//! The deserializable UI input contains only a workspace and optional manifest
//! place key. Executable paths, checksums, versions, ports, restart policy, and
//! process identifiers are derived or held by the Rust backend.

#![allow(clippy::missing_errors_doc)]

use crate::core::{OwnershipConfig, ProjectManifest, ValidationReport};
use crate::git_rojo::{
    IntegrationError, ProcessLimits, RojoPin, RojoSessionStartRequest, VerifiedRojo,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MAX_CONFIGURATION_BYTES: u64 = 1_048_576;
const DEFAULT_MAX_RESTARTS: u32 = 3;
const DEFAULT_LOG_LIMIT_BYTES: usize = 256 * 1024;
const MAX_ROJO_PROJECT_FILES: usize = 64;
const MAX_ROJO_PROJECT_NODES: usize = 16_384;
const MAX_ROJO_SOURCE_ENTRIES: usize = 100_000;

/// Result type for desktop Rojo configuration resolution.
pub type Result<T> = std::result::Result<T, RojoDesktopConfigError>;

/// Stable, UI-safe adapter error.
#[derive(Clone, Debug, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct RojoDesktopConfigError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Bounded human-readable explanation without lockfile contents.
    pub message: String,
}

impl RojoDesktopConfigError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 4096 {
            message.truncate(4096);
            message.push_str(" [truncated]");
        }
        Self { code, message }
    }

    fn from_integration(error: IntegrationError) -> Self {
        Self::new(error.code, error.message)
    }
}

/// The complete webview-supplied surface for live Rojo configuration.
///
/// Unknown fields are rejected, so an executable, checksum, port, or PID cannot
/// be smuggled into this boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RojoDesktopSelection {
    /// Workspace selected through the desktop's trusted workspace flow.
    pub workspace_root: PathBuf,
    /// Explicit manifest place key. When absent, `project.defaultPlace` is required.
    #[serde(default)]
    pub place_key: Option<String>,
}

/// One ownership-reconciled Rojo mutation scope.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoOwnershipMapping {
    /// Absolute escaped DataModel path controlled by this mapping.
    pub destination: String,
    /// Workspace-relative source, when the claim originates from `$path`.
    pub source: Option<PathBuf>,
    /// `subtree` for filesystem/destructive mappings or `exact` for properties.
    pub scope: String,
    /// Manifest ownership entry that authorizes the mapping.
    pub owner_id: String,
    /// Whether an explicitly approved ambiguous binary model was observed.
    pub binary_model: bool,
}

/// Rust-derived evidence that a Rojo project cannot manage Studio-owned roots.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoOwnershipReport {
    /// Canonical project files parsed, represented relative to the workspace.
    pub project_files: Vec<PathBuf>,
    /// Every effective mutation scope reconciled to a `git-code` owner.
    pub mappings: Vec<RojoOwnershipMapping>,
}

#[derive(Clone, Debug)]
enum ToolRootPolicy {
    WorkspaceRelative(PathBuf),
    TrustedFixed(PathBuf),
}

/// Rust-side policy adapter. This type is intentionally not deserializable.
#[derive(Clone, Debug)]
pub struct RojoDesktopConfigAdapter {
    tool_root: ToolRootPolicy,
    max_restarts: u32,
    log_limit_bytes: usize,
    verification_limits: ProcessLimits,
}

impl Default for RojoDesktopConfigAdapter {
    fn default() -> Self {
        Self::workspace_scoped()
    }
}

impl RojoDesktopConfigAdapter {
    /// Use `<workspace>/.neuman/tools` as the trusted artifact root.
    #[must_use]
    pub fn workspace_scoped() -> Self {
        Self {
            tool_root: ToolRootPolicy::WorkspaceRelative(PathBuf::from(".neuman/tools")),
            max_restarts: DEFAULT_MAX_RESTARTS,
            log_limit_bytes: DEFAULT_LOG_LIMIT_BYTES,
            verification_limits: ProcessLimits::default(),
        }
    }

    /// Use a preselected application-managed tool root outside the workspace.
    pub fn with_trusted_tool_root(tool_root: &Path) -> Result<Self> {
        let tool_root = canonical_directory(tool_root, "ROJO_TOOL_ROOT_INVALID")?;
        Ok(Self {
            tool_root: ToolRootPolicy::TrustedFixed(tool_root),
            ..Self::workspace_scoped()
        })
    }

    /// Set bounded supervisor policy from Rust-side application configuration.
    pub fn with_session_policy(
        mut self,
        max_restarts: u32,
        log_limit_bytes: usize,
    ) -> Result<Self> {
        if max_restarts > 32 {
            return Err(RojoDesktopConfigError::new(
                "ROJO_SESSION_POLICY_INVALID",
                "max restarts must be in 0..=32",
            ));
        }
        if !(4096..=4 * 1024 * 1024).contains(&log_limit_bytes) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_SESSION_POLICY_INVALID",
                "log limit must be in 4096..=4194304 bytes per stream",
            ));
        }
        self.max_restarts = max_restarts;
        self.log_limit_bytes = log_limit_bytes;
        Ok(self)
    }

    /// Set bounded executable-version verification limits from Rust-side policy.
    #[must_use]
    pub fn with_verification_limits(mut self, limits: ProcessLimits) -> Self {
        self.verification_limits = limits;
        self
    }

    /// Resolve, validate, and verify a selected workspace into a manager request.
    pub fn resolve(&self, selection: &RojoDesktopSelection) -> Result<ResolvedRojoDesktopConfig> {
        let workspace_root =
            canonical_directory(&selection.workspace_root, "ROJO_WORKSPACE_INVALID")?;
        let manifest_path = canonical_regular_file_inside(
            &workspace_root,
            &workspace_root.join("neuman.project.yaml"),
            "ROJO_MANIFEST_INVALID",
        )?;
        let (manifest, report) =
            ProjectManifest::load(&workspace_root).map_err(|report| manifest_error(&report))?;
        let manifest_hash = report.manifest_hash.ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_MANIFEST_INVALID",
                "validated manifest did not produce a canonical hash",
            )
        })?;
        let place_key = select_place(&manifest, selection.place_key.as_deref())?;

        let project_relative = validated_relative_path(
            &manifest.repository.project_file,
            "ROJO_PROJECT_PATH_INVALID",
        )?;
        let project_candidate = workspace_root.join(&project_relative);
        canonical_regular_file_inside(
            &workspace_root,
            &project_candidate,
            "ROJO_PROJECT_PATH_INVALID",
        )?;
        let ownership_report = preflight_rojo_ownership(
            &workspace_root,
            &project_candidate,
            &manifest.places[&place_key].ownership,
        )?;

        let lock_path = canonical_regular_file_inside(
            &workspace_root,
            &workspace_root.join("neuman.lock.json"),
            "ROJO_LOCK_INVALID",
        )?;
        let lock = read_bounded_json(&lock_path)?;
        validate_lock_manifest(&lock, &manifest_hash.to_string())?;
        let platform_key = current_rojo_platform_key()?;
        let locked = locked_rojo(&lock, &platform_key)?;
        validate_manifest_rojo_version(&manifest, &locked.version)?;

        let tool_root = self.resolve_tool_root(&workspace_root)?;
        let artifact_relative =
            validated_relative_path(&locked.artifact_path, "ROJO_LOCK_ARTIFACT_PATH_INVALID")?;
        let executable = canonical_regular_file_inside(
            &tool_root,
            &tool_root.join(&artifact_relative),
            "ROJO_LOCK_ARTIFACT_PATH_INVALID",
        )?;
        let pin = RojoPin {
            executable,
            version: locked.version,
            sha256: locked.sha256,
        };
        VerifiedRojo::verify(&pin, self.verification_limits)
            .map_err(RojoDesktopConfigError::from_integration)?;

        let request = RojoSessionStartRequest {
            pin,
            workspace_root: workspace_root.clone(),
            project_relative,
            place_key: place_key.clone(),
            max_restarts: self.max_restarts,
            log_limit_bytes: self.log_limit_bytes,
        };
        Ok(ResolvedRojoDesktopConfig {
            request,
            workspace_root,
            manifest_path,
            lock_path,
            tool_root,
            place_key,
            platform_key,
            ownership_report,
        })
    }

    fn resolve_tool_root(&self, workspace_root: &Path) -> Result<PathBuf> {
        match &self.tool_root {
            ToolRootPolicy::WorkspaceRelative(relative) => {
                let root =
                    canonical_directory(&workspace_root.join(relative), "ROJO_TOOL_ROOT_INVALID")?;
                if !root.starts_with(workspace_root) {
                    return Err(RojoDesktopConfigError::new(
                        "ROJO_TOOL_ROOT_INVALID",
                        "workspace tool root resolves outside the selected workspace",
                    ));
                }
                Ok(root)
            }
            ToolRootPolicy::TrustedFixed(root) => {
                canonical_directory(root, "ROJO_TOOL_ROOT_INVALID")
            }
        }
    }
}

/// Rust-only resolved configuration. It is deliberately not serializable.
#[derive(Clone, Debug)]
pub struct ResolvedRojoDesktopConfig {
    request: RojoSessionStartRequest,
    workspace_root: PathBuf,
    manifest_path: PathBuf,
    lock_path: PathBuf,
    tool_root: PathBuf,
    place_key: String,
    platform_key: String,
    ownership_report: RojoOwnershipReport,
}

impl ResolvedRojoDesktopConfig {
    /// Borrow the fully derived request for `RojoSessionManager::start`.
    #[must_use]
    pub fn session_request(&self) -> &RojoSessionStartRequest {
        &self.request
    }

    /// Consume the adapter result into a manager request.
    #[must_use]
    pub fn into_session_request(self) -> RojoSessionStartRequest {
        self.request
    }

    /// Canonical selected workspace.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Canonical validated manifest path.
    #[must_use]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Canonical validated lockfile path.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Canonical trusted tool root.
    #[must_use]
    pub fn tool_root(&self) -> &Path {
        &self.tool_root
    }

    /// Selected valid manifest place key.
    #[must_use]
    pub fn place_key(&self) -> &str {
        &self.place_key
    }

    /// Lockfile platform artifact key selected by this host.
    #[must_use]
    pub fn platform_key(&self) -> &str {
        &self.platform_key
    }

    /// Ownership reconciliation evidence produced before the manager can start.
    #[must_use]
    pub fn ownership_report(&self) -> &RojoOwnershipReport {
        &self.ownership_report
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MappingScope {
    Exact,
    Subtree,
}

impl MappingScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Subtree => "subtree",
        }
    }
}

struct RojoOwnershipPreflight<'a> {
    workspace_root: &'a Path,
    ownership: &'a [OwnershipConfig],
    active_projects: BTreeSet<PathBuf>,
    parsed_projects: BTreeSet<PathBuf>,
    project_files: Vec<PathBuf>,
    mappings: Vec<RojoOwnershipMapping>,
    node_count: usize,
    source_entry_count: usize,
}

fn preflight_rojo_ownership(
    workspace_root: &Path,
    project_path: &Path,
    ownership: &[OwnershipConfig],
) -> Result<RojoOwnershipReport> {
    let mut preflight = RojoOwnershipPreflight {
        workspace_root,
        ownership,
        active_projects: BTreeSet::new(),
        parsed_projects: BTreeSet::new(),
        project_files: Vec::new(),
        mappings: Vec::new(),
        node_count: 0,
        source_entry_count: 0,
    };
    preflight.visit_project(project_path, "/")?;
    if preflight.mappings.is_empty() {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_NO_MAPPINGS",
            "Rojo project has no ownership-reconciled mutation mappings",
        ));
    }
    Ok(RojoOwnershipReport {
        project_files: preflight.project_files,
        mappings: preflight.mappings,
    })
}

impl RojoOwnershipPreflight<'_> {
    fn visit_project(&mut self, candidate: &Path, destination: &str) -> Result<()> {
        let project = self.secure_source(candidate, true)?;
        if !is_project_json(&project) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_UNSUPPORTED",
                "project includes must use the exact .project.json format; JSONC is not accepted by the ownership preflight",
            ));
        }
        if self.active_projects.contains(&project) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INCLUDE_CYCLE",
                "Rojo project includes form a cycle",
            ));
        }
        let first_visit = !self.parsed_projects.contains(&project);
        if first_visit && self.parsed_projects.len() >= MAX_ROJO_PROJECT_FILES {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_LIMIT_EXCEEDED",
                format!("Rojo project include count exceeds {MAX_ROJO_PROJECT_FILES}"),
            ));
        }
        let metadata = fs::metadata(&project).map_err(|error| {
            RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", error.to_string())
        })?;
        if metadata.len() > MAX_CONFIGURATION_BYTES {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                "Rojo project file exceeds 1 MiB",
            ));
        }
        let bytes = fs::read(&project).map_err(|error| {
            RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", error.to_string())
        })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                format!("Rojo project must be strict JSON: {error}"),
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", "project root must be an object")
        })?;
        if let Some(field) = object.keys().find(|field| {
            !matches!(
                field.as_str(),
                "name"
                    | "tree"
                    | "servePort"
                    | "servePlaceIds"
                    | "placeId"
                    | "gameId"
                    | "serveAddress"
                    | "globIgnorePaths"
                    | "emitLegacyScripts"
            )
        }) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_UNSUPPORTED",
                format!(
                    "unsupported Rojo project field `{field}` could change effective filesystem mappings"
                ),
            ));
        }
        let name = object.get("name").and_then(Value::as_str).ok_or_else(|| {
            RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", "project name must be a string")
        })?;
        if name.trim().is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                "project name must be non-empty, printable, and at most 200 bytes",
            ));
        }
        validate_project_options(object)?;
        let tree = object
            .get("tree")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                RojoDesktopConfigError::new(
                    "ROJO_PROJECT_INVALID",
                    "project tree must be an instance-description object",
                )
            })?;

        self.active_projects.insert(project.clone());
        self.parsed_projects.insert(project.clone());
        if first_visit {
            self.project_files.push(self.relative_source(&project)?);
        }
        let base = project.parent().ok_or_else(|| {
            RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", "project has no parent directory")
        })?;
        let result = self.visit_node(tree, destination, base, destination == "/");
        self.active_projects.remove(&project);
        result
    }

    fn visit_node(
        &mut self,
        node: &Map<String, Value>,
        destination: &str,
        project_directory: &Path,
        project_root: bool,
    ) -> Result<()> {
        self.node_count += 1;
        if self.node_count > MAX_ROJO_PROJECT_NODES {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_LIMIT_EXCEEDED",
                format!("Rojo instance-description count exceeds {MAX_ROJO_PROJECT_NODES}"),
            ));
        }
        if let Some(directive) = node.keys().find(|key| {
            key.starts_with('$')
                && !matches!(
                    key.as_str(),
                    "$className"
                        | "$path"
                        | "$properties"
                        | "$attributes"
                        | "$ignoreUnknownInstances"
                )
        }) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_UNSUPPORTED",
                format!("unsupported Rojo instance directive `{directive}`"),
            ));
        }
        let class_name = optional_string(node, "$className")?;
        let path_value = optional_string(node, "$path")?;
        require_optional_object(node, "$properties")?;
        require_optional_object(node, "$attributes")?;
        let ignore_unknown = optional_bool(node, "$ignoreUnknownInstances")?;
        if class_name.is_none()
            && path_value.is_none()
            && node.keys().all(|key| key.starts_with('$'))
        {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                format!("instance description at `{destination}` has no class, path, or children"),
            ));
        }

        let has_properties = node.contains_key("$properties") || node.contains_key("$attributes");
        let effective_ignore_unknown = ignore_unknown.unwrap_or(path_value.is_none());
        if effective_ignore_unknown {
            self.record_mapping(destination, None, MappingScope::Subtree, false)?;
        }
        if has_properties {
            self.record_mapping(destination, None, MappingScope::Exact, false)?;
        }
        if !project_root
            && path_value.is_none()
            && !is_pass_through_service(destination, class_name, ignore_unknown, has_properties)
        {
            self.record_mapping(destination, None, MappingScope::Exact, false)?;
        }

        if let Some(path_value) = path_value {
            let relative = validated_relative_path(path_value, "ROJO_PROJECT_PATH_ESCAPE")?;
            let source = self.secure_source(&project_directory.join(relative), false)?;
            if source.is_dir() {
                let default_project = source.join("default.project.json");
                if default_project.is_file() {
                    self.visit_project(&default_project, destination)?;
                } else {
                    let contains_binary = self.scan_source_tree(&source, destination)?;
                    self.record_mapping(
                        destination,
                        Some(&source),
                        MappingScope::Subtree,
                        contains_binary,
                    )?;
                }
            } else if is_project_json(&source) {
                self.visit_project(&source, destination)?;
            } else {
                if is_project_jsonc(&source) {
                    return Err(RojoDesktopConfigError::new(
                        "ROJO_PROJECT_UNSUPPORTED",
                        "JSONC project includes are not accepted by the ownership preflight",
                    ));
                }
                if !is_supported_rojo_source(&source) {
                    return Err(RojoDesktopConfigError::new(
                        "ROJO_PROJECT_UNSUPPORTED",
                        format!(
                            "unsupported Rojo source type at `{}`",
                            self.relative_source(&source)?.display()
                        ),
                    ));
                }
                self.record_mapping(
                    destination,
                    Some(&source),
                    MappingScope::Subtree,
                    is_binary_model(&source),
                )?;
            }
        }

        for (name, child) in node.iter().filter(|(key, _)| !key.starts_with('$')) {
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(RojoDesktopConfigError::new(
                    "ROJO_PROJECT_INVALID",
                    "instance names must be non-empty and printable",
                ));
            }
            let child = child.as_object().ok_or_else(|| {
                RojoDesktopConfigError::new(
                    "ROJO_PROJECT_INVALID",
                    format!("child `{name}` must be an instance-description object"),
                )
            })?;
            let child_destination = join_data_model_path(destination, name);
            self.visit_node(child, &child_destination, project_directory, false)?;
        }
        Ok(())
    }

    fn scan_source_tree(&mut self, root: &Path, destination: &str) -> Result<bool> {
        let mut stack = vec![root.to_path_buf()];
        let mut contains_binary = false;
        while let Some(directory) = stack.pop() {
            let entries = fs::read_dir(&directory).map_err(|error| {
                RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", error.to_string())
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", error.to_string())
                })?;
                self.source_entry_count += 1;
                if self.source_entry_count > MAX_ROJO_SOURCE_ENTRIES {
                    return Err(RojoDesktopConfigError::new(
                        "ROJO_PROJECT_LIMIT_EXCEEDED",
                        format!("Rojo source entry count exceeds {MAX_ROJO_SOURCE_ENTRIES}"),
                    ));
                }
                let file_type = entry.file_type().map_err(|error| {
                    RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", error.to_string())
                })?;
                if file_type.is_symlink() {
                    return Err(RojoDesktopConfigError::new(
                        "ROJO_PROJECT_SYMLINK_FORBIDDEN",
                        format!(
                            "Rojo source tree contains symlink `{}`",
                            self.relative_source(&entry.path())?.display()
                        ),
                    ));
                }
                let path = entry.path();
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() {
                    contains_binary |= is_binary_model(&path);
                    if is_project_jsonc(&path) {
                        return Err(RojoDesktopConfigError::new(
                            "ROJO_PROJECT_UNSUPPORTED",
                            "nested .project.jsonc files require unsupported JSONC resolution",
                        ));
                    }
                    if is_project_json(&path) {
                        self.visit_project(&path, destination)?;
                    }
                } else {
                    return Err(RojoDesktopConfigError::new(
                        "ROJO_PROJECT_UNSUPPORTED",
                        "Rojo source contains a non-file, non-directory entry",
                    ));
                }
            }
        }
        Ok(contains_binary)
    }

    fn record_mapping(
        &mut self,
        destination: &str,
        source: Option<&Path>,
        scope: MappingScope,
        binary_model: bool,
    ) -> Result<()> {
        let owner = self.owner_for(destination, scope)?;
        if let Some(source) = source {
            let project_path = owner
                .options
                .get("projectPath")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    RojoDesktopConfigError::new(
                        "ROJO_OWNERSHIP_SOURCE_MISMATCH",
                        format!(
                            "git-code owner `{}` must declare projectPath for `$path` mappings",
                            owner.id
                        ),
                    )
                })?;
            let relative = validated_relative_path(project_path, "ROJO_OWNERSHIP_SOURCE_MISMATCH")?;
            let owned_source = self.secure_source(&self.workspace_root.join(relative), false)?;
            if !source.starts_with(&owned_source) {
                return Err(RojoDesktopConfigError::new(
                    "ROJO_OWNERSHIP_SOURCE_MISMATCH",
                    format!(
                        "Rojo source `{}` is outside projectPath for owner `{}`",
                        self.relative_source(source)?.display(),
                        owner.id
                    ),
                ));
            }
        }
        if binary_model
            && owner
                .options
                .get("allowRojoBinaryModels")
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Err(RojoDesktopConfigError::new(
                "ROJO_BINARY_MODEL_OVERRIDE_REQUIRED",
                format!(
                    "git-code owner `{}` must explicitly set allowRojoBinaryModels: true before Rojo can manage .rbxm/.rbxmx content",
                    owner.id
                ),
            ));
        }
        let source = source
            .map(|value| self.relative_source(value))
            .transpose()?;
        let mapping = RojoOwnershipMapping {
            destination: destination.to_owned(),
            source,
            scope: scope.as_str().to_owned(),
            owner_id: owner.id.clone(),
            binary_model,
        };
        if !self.mappings.iter().any(|existing| {
            existing.destination == mapping.destination
                && existing.source == mapping.source
                && existing.scope == mapping.scope
                && existing.owner_id == mapping.owner_id
                && existing.binary_model == mapping.binary_model
        }) {
            self.mappings.push(mapping);
        }
        Ok(())
    }

    fn owner_for(&self, destination: &str, scope: MappingScope) -> Result<&OwnershipConfig> {
        if scope == MappingScope::Subtree
            && let Some(child) = self
                .ownership
                .iter()
                .find(|candidate| data_model_contains(destination, &candidate.path))
            && !self
                .ownership
                .iter()
                .any(|candidate| data_model_contains(&candidate.path, destination))
        {
            return Err(RojoDesktopConfigError::new(
                "ROJO_OWNERSHIP_CONFLICT",
                format!(
                    "Rojo subtree `{destination}` is an ancestor of {} owner `{}` at `{}`",
                    child.owner, child.id, child.path
                ),
            ));
        }
        let mut containing = self
            .ownership
            .iter()
            .filter(|owner| data_model_contains(&owner.path, destination))
            .collect::<Vec<_>>();
        containing.sort_by_key(|owner| owner.path.len());
        let owner = containing.last().copied().ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_OWNERSHIP_UNDECLARED",
                format!("Rojo mutation at `{destination}` has no declared owner"),
            )
        })?;
        if owner.owner != "git-code" {
            return Err(RojoDesktopConfigError::new(
                "ROJO_OWNERSHIP_CONFLICT",
                format!(
                    "Rojo mutation at `{destination}` overlaps {} owner `{}`",
                    owner.owner, owner.id
                ),
            ));
        }
        if scope == MappingScope::Subtree
            && let Some(child) = self.ownership.iter().find(|candidate| {
                candidate.id != owner.id && data_model_contains(destination, &candidate.path)
            })
        {
            return Err(RojoDesktopConfigError::new(
                "ROJO_OWNERSHIP_CONFLICT",
                format!(
                    "Rojo subtree `{destination}` crosses delegated {} owner `{}` at `{}`",
                    child.owner, child.id, child.path
                ),
            ));
        }
        Ok(owner)
    }

    fn secure_source(&self, candidate: &Path, require_file: bool) -> Result<PathBuf> {
        reject_symlink_components(self.workspace_root, candidate)?;
        let source = fs::canonicalize(candidate).map_err(|error| {
            RojoDesktopConfigError::new("ROJO_PROJECT_PATH_ESCAPE", error.to_string())
        })?;
        if !source.starts_with(self.workspace_root) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_PATH_ESCAPE",
                "Rojo source resolves outside the selected workspace",
            ));
        }
        let metadata = fs::metadata(&source).map_err(|error| {
            RojoDesktopConfigError::new("ROJO_PROJECT_INVALID", error.to_string())
        })?;
        if require_file && !metadata.is_file() {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                "Rojo project include is not a regular file",
            ));
        }
        if !require_file && !metadata.is_file() && !metadata.is_dir() {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                "Rojo source is not a regular file or directory",
            ));
        }
        Ok(source)
    }

    fn relative_source(&self, source: &Path) -> Result<PathBuf> {
        source
            .strip_prefix(self.workspace_root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                RojoDesktopConfigError::new(
                    "ROJO_PROJECT_PATH_ESCAPE",
                    "Rojo source is outside the selected workspace",
                )
            })
    }
}

fn validate_project_options(project: &Map<String, Value>) -> Result<()> {
    if project.get("servePort").is_some_and(|value| {
        value
            .as_u64()
            .is_none_or(|port| port == 0 || port > u64::from(u16::MAX))
    }) {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_INVALID",
            "servePort must be a nonzero u16 when present",
        ));
    }
    if project
        .get("servePlaceIds")
        .is_some_and(|value| !value.is_null() && !value.is_array())
    {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_INVALID",
            "servePlaceIds must be an array or null",
        ));
    }
    for field in ["placeId", "gameId"] {
        if project
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string() && !value.is_u64())
        {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_INVALID",
                format!("{field} must be a string, unsigned integer, or null"),
            ));
        }
    }
    if project
        .get("serveAddress")
        .is_some_and(|value| !value.is_null())
    {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_UNSUPPORTED",
            "serveAddress is backend-owned and must be omitted from desktop projects",
        ));
    }
    if project
        .get("globIgnorePaths")
        .is_some_and(|value| value.as_array().is_none_or(|paths| !paths.is_empty()))
    {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_UNSUPPORTED",
            "non-empty globIgnorePaths makes ownership resolution dynamic and is not accepted",
        ));
    }
    if project
        .get("emitLegacyScripts")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_INVALID",
            "emitLegacyScripts must be boolean",
        ));
    }
    Ok(())
}

fn optional_string<'a>(node: &'a Map<String, Value>, field: &str) -> Result<Option<&'a str>> {
    node.get(field)
        .map(|value| {
            value.as_str().ok_or_else(|| {
                RojoDesktopConfigError::new(
                    "ROJO_PROJECT_INVALID",
                    format!("{field} must be a string"),
                )
            })
        })
        .transpose()
}

fn optional_bool(node: &Map<String, Value>, field: &str) -> Result<Option<bool>> {
    node.get(field)
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                RojoDesktopConfigError::new(
                    "ROJO_PROJECT_INVALID",
                    format!("{field} must be boolean"),
                )
            })
        })
        .transpose()
}

fn require_optional_object(node: &Map<String, Value>, field: &str) -> Result<()> {
    if node.get(field).is_some_and(|value| !value.is_object()) {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PROJECT_INVALID",
            format!("{field} must be an object"),
        ));
    }
    Ok(())
}

fn reject_symlink_components(workspace_root: &Path, candidate: &Path) -> Result<()> {
    let relative = candidate.strip_prefix(workspace_root).map_err(|_| {
        RojoDesktopConfigError::new(
            "ROJO_PROJECT_PATH_ESCAPE",
            "Rojo source path is not lexically inside the workspace",
        )
    })?;
    let mut current = workspace_root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_PATH_ESCAPE",
                "Rojo source path contains a non-normal component",
            ));
        }
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            RojoDesktopConfigError::new("ROJO_PROJECT_PATH_ESCAPE", error.to_string())
        })?;
        if metadata.file_type().is_symlink() {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PROJECT_SYMLINK_FORBIDDEN",
                "Rojo project and source paths may not traverse symlinks",
            ));
        }
    }
    Ok(())
}

fn is_pass_through_service(
    destination: &str,
    class_name: Option<&str>,
    ignore_unknown: Option<bool>,
    has_properties: bool,
) -> bool {
    if ignore_unknown != Some(false) || has_properties {
        return false;
    }
    let Some(name) = destination
        .strip_prefix('/')
        .filter(|name| !name.contains('/'))
    else {
        return false;
    };
    class_name.is_none_or(|class| class == name) && is_known_service(name)
}

fn is_known_service(name: &str) -> bool {
    matches!(
        name,
        "Workspace"
            | "ReplicatedFirst"
            | "ReplicatedStorage"
            | "ServerScriptService"
            | "ServerStorage"
            | "StarterGui"
            | "StarterPack"
            | "StarterPlayer"
            | "Lighting"
            | "SoundService"
            | "Chat"
            | "Teams"
            | "LocalizationService"
            | "TextChatService"
            | "MaterialService"
    )
}

fn join_data_model_path(parent: &str, name: &str) -> String {
    let segment = name.replace('~', "~0").replace('/', "~1");
    if parent == "/" {
        format!("/{segment}")
    } else {
        format!("{parent}/{segment}")
    }
}

fn data_model_contains(root: &str, path: &str) -> bool {
    root == "/"
        || root == path
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn is_project_json(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".project.json"))
}

fn is_project_jsonc(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".project.jsonc"))
}

fn is_binary_model(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "rbxm" | "rbxmx")
        })
}

fn is_supported_rojo_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    [
        ".lua", ".luau", ".json", ".toml", ".txt", ".csv", ".rbxm", ".rbxmx",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

#[derive(Clone, Debug)]
struct LockedRojo {
    version: String,
    sha256: String,
    artifact_path: String,
}

fn select_place(manifest: &ProjectManifest, requested: Option<&str>) -> Result<String> {
    let selected = match requested {
        Some(value) if value.trim().is_empty() => {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PLACE_INVALID",
                "explicit place key must not be empty",
            ));
        }
        Some(value) => value,
        None => manifest.project.default_place.as_deref().ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_PLACE_REQUIRED",
                "choose an explicit place because project.defaultPlace is not set",
            )
        })?,
    };
    if !manifest.places.contains_key(selected) {
        return Err(RojoDesktopConfigError::new(
            "ROJO_PLACE_UNKNOWN",
            format!("place key `{selected}` does not exist in the validated manifest"),
        ));
    }
    Ok(selected.to_owned())
}

fn manifest_error(report: &ValidationReport) -> RojoDesktopConfigError {
    let mut messages = report
        .errors
        .iter()
        .take(8)
        .map(|issue| format!("{} at {}: {}", issue.code, issue.path, issue.message))
        .collect::<Vec<_>>();
    if report.errors.len() > messages.len() {
        messages.push(format!(
            "{} additional manifest errors",
            report.errors.len() - messages.len()
        ));
    }
    RojoDesktopConfigError::new("ROJO_MANIFEST_INVALID", messages.join("; "))
}

fn read_bounded_json(path: &Path) -> Result<Value> {
    let metadata = fs::metadata(path)
        .map_err(|error| RojoDesktopConfigError::new("ROJO_LOCK_INVALID", error.to_string()))?;
    if metadata.len() > MAX_CONFIGURATION_BYTES {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_INVALID",
            "neuman.lock.json exceeds 1 MiB",
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| RojoDesktopConfigError::new("ROJO_LOCK_INVALID", error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        RojoDesktopConfigError::new("ROJO_LOCK_INVALID", format!("invalid JSON: {error}"))
    })
}

fn validate_lock_manifest(lock: &Value, expected_manifest_hash: &str) -> Result<()> {
    let root = lock.as_object().ok_or_else(|| {
        RojoDesktopConfigError::new("ROJO_LOCK_INVALID", "lockfile root must be an object")
    })?;
    if root.get("schemaVersion").and_then(Value::as_str) != Some("1.0") {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_INVALID",
            "lockfile schemaVersion must be exactly 1.0",
        ));
    }
    let locked_manifest_hash = root
        .get("manifestHash")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_LOCK_MANIFEST_MISMATCH",
                "lockfile manifestHash is required",
            )
        })?;
    if locked_manifest_hash != expected_manifest_hash {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_MANIFEST_MISMATCH",
            "lockfile was not generated for the selected manifest",
        ));
    }
    Ok(())
}

fn locked_rojo(lock: &Value, primary_platform: &str) -> Result<LockedRojo> {
    let rojo = lock
        .get("toolchain")
        .and_then(Value::as_object)
        .and_then(|toolchain| toolchain.get("rojo"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_LOCK_ENTRY_INVALID",
                "lockfile toolchain.rojo object is required",
            )
        })?;
    let version = rojo.get("version").and_then(Value::as_str).ok_or_else(|| {
        RojoDesktopConfigError::new(
            "ROJO_LOCK_VERSION_INVALID",
            "toolchain.rojo.version must be an exact string",
        )
    })?;
    validate_exact_version(version)?;

    let root_sha = rojo.get("sha256").map(parse_sha256_value).transpose()?;
    let candidates = platform_candidates(primary_platform);
    let platform_artifact =
        rojo.get("artifacts")
            .and_then(Value::as_object)
            .and_then(|artifacts| {
                candidates
                    .iter()
                    .find_map(|candidate| artifacts.get(candidate).map(|value| (candidate, value)))
            });

    let (artifact_path, sha256) = if let Some((_, artifact)) = platform_artifact {
        parse_artifact(artifact, root_sha.as_deref())?
    } else {
        let path = rojo.get("path").and_then(Value::as_str).ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_LOCK_ARTIFACT_MISSING",
                format!(
                    "no artifact for platform `{primary_platform}` and no trusted fallback path"
                ),
            )
        })?;
        let sha = root_sha.ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_LOCK_SHA256_INVALID",
                "fallback Rojo artifact requires toolchain.rojo.sha256",
            )
        })?;
        (path.to_owned(), sha)
    };
    Ok(LockedRojo {
        version: version.to_owned(),
        sha256,
        artifact_path,
    })
}

fn parse_artifact(artifact: &Value, fallback_sha: Option<&str>) -> Result<(String, String)> {
    match artifact {
        Value::String(path) => {
            let sha = fallback_sha.ok_or_else(|| {
                RojoDesktopConfigError::new(
                    "ROJO_LOCK_SHA256_INVALID",
                    "string artifact requires toolchain.rojo.sha256",
                )
            })?;
            Ok((path.clone(), sha.to_owned()))
        }
        Value::Object(object) => {
            reject_unknown_artifact_fields(object)?;
            let path = object.get("path").and_then(Value::as_str).ok_or_else(|| {
                RojoDesktopConfigError::new(
                    "ROJO_LOCK_ARTIFACT_PATH_INVALID",
                    "platform artifact path must be a string",
                )
            })?;
            let sha = match object.get("sha256") {
                Some(value) => parse_sha256_value(value)?,
                None => fallback_sha
                    .ok_or_else(|| {
                        RojoDesktopConfigError::new(
                            "ROJO_LOCK_SHA256_INVALID",
                            "platform artifact requires sha256",
                        )
                    })?
                    .to_owned(),
            };
            Ok((path.to_owned(), sha))
        }
        _ => Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_ARTIFACT_PATH_INVALID",
            "platform artifact must be a relative path string or {path, sha256} object",
        )),
    }
}

fn reject_unknown_artifact_fields(object: &Map<String, Value>) -> Result<()> {
    if let Some(field) = object
        .keys()
        .find(|field| !matches!(field.as_str(), "path" | "sha256"))
    {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_ENTRY_INVALID",
            format!("unknown platform artifact field `{field}`"),
        ));
    }
    Ok(())
}

fn validate_manifest_rojo_version(manifest: &ProjectManifest, locked_version: &str) -> Result<()> {
    let manifest_version = manifest
        .toolchain
        .rojo
        .as_object()
        .and_then(|rojo| rojo.get("version"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RojoDesktopConfigError::new(
                "ROJO_MANIFEST_VERSION_INVALID",
                "toolchain.rojo.version must be an exact string",
            )
        })?;
    validate_exact_version(manifest_version)?;
    if manifest_version != locked_version {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_VERSION_MISMATCH",
            "manifest and lockfile select different exact Rojo versions",
        ));
    }
    Ok(())
}

fn validate_exact_version(version: &str) -> Result<()> {
    if version.is_empty() || version.len() > 64 || !version.is_ascii() {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_VERSION_INVALID",
            "Rojo version must be an exact ASCII semantic version",
        ));
    }
    let without_build = version.split_once('+').map_or(version, |(core, build)| {
        if valid_semver_identifier_list(build) {
            core
        } else {
            ""
        }
    });
    let (core, prerelease_valid) = without_build
        .split_once('-')
        .map_or((without_build, true), |(core, suffix)| {
            (core, valid_semver_identifier_list(suffix))
        });
    let components: Vec<_> = core.split('.').collect();
    let numeric_core = components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == &"0" || !component.starts_with('0'))
        });
    if !numeric_core || !prerelease_valid {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_VERSION_INVALID",
            "Rojo version must be exact MAJOR.MINOR.PATCH with valid optional prerelease/build identifiers",
        ));
    }
    Ok(())
}

fn valid_semver_identifier_list(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn parse_sha256_value(value: &Value) -> Result<String> {
    let value = value.as_str().ok_or_else(|| {
        RojoDesktopConfigError::new("ROJO_LOCK_SHA256_INVALID", "sha256 must be a string")
    })?;
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(RojoDesktopConfigError::new(
            "ROJO_LOCK_SHA256_INVALID",
            "sha256 must contain exactly 64 lowercase hexadecimal characters",
        ));
    }
    Ok(digest.to_owned())
}

fn validated_relative_path(value: &str, code: &'static str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RojoDesktopConfigError::new(
            code,
            "path must be a normalized non-empty relative path without traversal",
        ));
    }
    Ok(path)
}

fn canonical_directory(path: &Path, code: &'static str) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| RojoDesktopConfigError::new(code, error.to_string()))?;
    if !fs::metadata(&canonical)
        .map_err(|error| RojoDesktopConfigError::new(code, error.to_string()))?
        .is_dir()
    {
        return Err(RojoDesktopConfigError::new(code, "path is not a directory"));
    }
    Ok(canonical)
}

fn canonical_regular_file_inside(
    root: &Path,
    candidate: &Path,
    code: &'static str,
) -> Result<PathBuf> {
    let root = canonical_directory(root, code)?;
    let candidate = fs::canonicalize(candidate)
        .map_err(|error| RojoDesktopConfigError::new(code, error.to_string()))?;
    if !candidate.starts_with(&root) {
        return Err(RojoDesktopConfigError::new(
            code,
            "path resolves outside its trusted root",
        ));
    }
    if !fs::metadata(&candidate)
        .map_err(|error| RojoDesktopConfigError::new(code, error.to_string()))?
        .is_file()
    {
        return Err(RojoDesktopConfigError::new(
            code,
            "path is not a regular file",
        ));
    }
    Ok(candidate)
}

/// Return the canonical lockfile platform key for the current build target.
pub fn current_rojo_platform_key() -> Result<String> {
    let key = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        (os, arch) => {
            return Err(RojoDesktopConfigError::new(
                "ROJO_PLATFORM_UNSUPPORTED",
                format!("no qualified Rojo artifact platform for {os}/{arch}"),
            ));
        }
    };
    Ok(key.to_owned())
}

fn platform_candidates(primary: &str) -> Vec<String> {
    vec![
        primary.to_owned(),
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::starter_manifest;
    use sha2::{Digest, Sha256};
    use std::process::Command;

    fn sha256_file(path: &Path) -> String {
        hex::encode(Sha256::digest(fs::read(path).unwrap()))
    }

    fn compile_fake_rojo(tool_root: &Path, relative: &Path) -> PathBuf {
        let executable = tool_root.join(relative);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let source = tool_root.join("fake_rojo_version.rs");
        fs::write(
            &source,
            r#"fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Rojo 7.7.0");
    } else {
        std::process::exit(2);
    }
}
"#,
        )
        .unwrap();
        let output = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    }

    fn workspace_fixture() -> (tempfile::TempDir, PathBuf, String) {
        let workspace = tempfile::tempdir().unwrap();
        let yaml = starter_manifest("adapter-test", "Adapter Test").unwrap();
        fs::write(workspace.path().join("neuman.project.yaml"), &yaml).unwrap();
        fs::create_dir_all(workspace.path().join("src/server")).unwrap();
        fs::write(
            workspace.path().join("src/server/Main.server.luau"),
            b"print('safe')",
        )
        .unwrap();
        fs::write(
            workspace.path().join("default.project.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "Adapter Test",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "ServerScriptService": {
                        "$className": "ServerScriptService",
                        "$path": "src/server"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let (_, report) = ProjectManifest::parse(yaml.as_bytes()).unwrap();
        let manifest_hash = report.manifest_hash.unwrap().to_string();
        let tool_root = workspace.path().join(".neuman/tools");
        fs::create_dir_all(&tool_root).unwrap();
        (workspace, tool_root, manifest_hash)
    }

    fn write_lock(workspace: &Path, manifest_hash: &str, artifact_relative: &Path, sha256: &str) {
        let platform = current_rojo_platform_key().unwrap();
        let lock = serde_json::json!({
            "schemaVersion": "1.0",
            "manifestHash": manifest_hash,
            "toolchain": {
                "rojo": {
                    "version": "7.7.0",
                    "sha256": format!("sha256:{sha256}"),
                    "path": "missing/fallback-rojo",
                    "artifacts": {
                        (platform): {
                            "path": artifact_relative.to_string_lossy(),
                            "sha256": format!("sha256:{sha256}")
                        }
                    }
                }
            }
        });
        fs::write(
            workspace.join("neuman.lock.json"),
            serde_json::to_vec_pretty(&lock).unwrap(),
        )
        .unwrap();
    }

    fn ownership_for(workspace: &Path, yaml: &str) -> Vec<OwnershipConfig> {
        fs::write(workspace.join("neuman.project.yaml"), yaml).unwrap();
        let (manifest, _) = ProjectManifest::parse(yaml.as_bytes()).unwrap();
        manifest.places["lobby"].ownership.clone()
    }

    fn write_rojo_project(workspace: &Path, value: &Value) -> PathBuf {
        let path = workspace.join("default.project.json");
        fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn resolves_default_and_explicit_place_from_platform_artifact() {
        let (workspace, tool_root, manifest_hash) = workspace_fixture();
        let relative = PathBuf::from(format!(
            "{}/rojo{}",
            current_rojo_platform_key().unwrap(),
            std::env::consts::EXE_SUFFIX
        ));
        let executable = compile_fake_rojo(&tool_root, &relative);
        let sha256 = sha256_file(&executable);
        write_lock(workspace.path(), &manifest_hash, &relative, &sha256);
        let adapter = RojoDesktopConfigAdapter::workspace_scoped();

        let default = adapter
            .resolve(&RojoDesktopSelection {
                workspace_root: workspace.path().to_path_buf(),
                place_key: None,
            })
            .unwrap();
        assert_eq!(default.place_key(), "lobby");
        assert_eq!(default.platform_key(), current_rojo_platform_key().unwrap());
        assert_eq!(default.tool_root(), fs::canonicalize(&tool_root).unwrap());
        assert_eq!(
            default.session_request().pin.executable,
            fs::canonicalize(&executable).unwrap()
        );
        assert_eq!(default.session_request().pin.sha256, sha256);
        assert_eq!(
            default.session_request().project_relative,
            PathBuf::from("default.project.json")
        );

        let explicit = adapter
            .resolve(&RojoDesktopSelection {
                workspace_root: workspace.path().to_path_buf(),
                place_key: Some("lobby".to_owned()),
            })
            .unwrap();
        assert_eq!(explicit.place_key(), "lobby");
    }

    #[test]
    fn webview_selection_rejects_process_and_executable_fields() {
        assert!(
            serde_json::from_value::<RojoDesktopSelection>(serde_json::json!({
                "workspaceRoot": "C:/selected",
                "placeKey": "lobby",
                "executable": "evil.exe"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RojoDesktopSelection>(serde_json::json!({
                "workspaceRoot": "C:/selected",
                "pid": 1234
            }))
            .is_err()
        );
    }

    #[test]
    fn rejects_missing_place_default_and_unknown_explicit_place() {
        let (workspace, tool_root, manifest_hash) = workspace_fixture();
        let yaml = fs::read_to_string(workspace.path().join("neuman.project.yaml"))
            .unwrap()
            .replace("  defaultPlace: lobby\n", "");
        fs::write(workspace.path().join("neuman.project.yaml"), &yaml).unwrap();
        let (_, report) = ProjectManifest::parse(yaml.as_bytes()).unwrap();
        let updated_manifest_hash = report.manifest_hash.unwrap().to_string();
        let relative = PathBuf::from(format!("rojo{}", std::env::consts::EXE_SUFFIX));
        let executable = compile_fake_rojo(&tool_root, &relative);
        write_lock(
            workspace.path(),
            &updated_manifest_hash,
            &relative,
            &sha256_file(&executable),
        );
        let adapter = RojoDesktopConfigAdapter::workspace_scoped();
        assert_eq!(
            adapter
                .resolve(&RojoDesktopSelection {
                    workspace_root: workspace.path().to_path_buf(),
                    place_key: None,
                })
                .unwrap_err()
                .code,
            "ROJO_PLACE_REQUIRED"
        );
        assert_eq!(
            adapter
                .resolve(&RojoDesktopSelection {
                    workspace_root: workspace.path().to_path_buf(),
                    place_key: Some("unknown".to_owned()),
                })
                .unwrap_err()
                .code,
            "ROJO_PLACE_UNKNOWN"
        );
        assert_ne!(manifest_hash, updated_manifest_hash);
    }

    #[test]
    fn rejects_manifest_mismatch_traversal_and_invalid_pin_shapes() {
        let (workspace, tool_root, manifest_hash) = workspace_fixture();
        let relative = PathBuf::from(format!("rojo{}", std::env::consts::EXE_SUFFIX));
        let executable = compile_fake_rojo(&tool_root, &relative);
        let sha256 = sha256_file(&executable);
        write_lock(
            workspace.path(),
            "b3-256:not-the-manifest",
            &relative,
            &sha256,
        );
        let adapter = RojoDesktopConfigAdapter::workspace_scoped();
        let selection = RojoDesktopSelection {
            workspace_root: workspace.path().to_path_buf(),
            place_key: None,
        };
        assert_eq!(
            adapter.resolve(&selection).unwrap_err().code,
            "ROJO_LOCK_MANIFEST_MISMATCH"
        );

        write_lock(
            workspace.path(),
            &manifest_hash,
            Path::new("../escape"),
            &sha256,
        );
        assert_eq!(
            adapter.resolve(&selection).unwrap_err().code,
            "ROJO_LOCK_ARTIFACT_PATH_INVALID"
        );

        let platform = current_rojo_platform_key().unwrap();
        let invalid = serde_json::json!({
            "schemaVersion": "1.0",
            "manifestHash": manifest_hash,
            "toolchain": {"rojo": {
                "version": "latest",
                "artifacts": {(platform): {"path": relative, "sha256": "sha256:ABC"}}
            }}
        });
        fs::write(
            workspace.path().join("neuman.lock.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert_eq!(
            adapter.resolve(&selection).unwrap_err().code,
            "ROJO_LOCK_VERSION_INVALID"
        );
    }

    #[test]
    fn fixed_tool_root_is_backend_policy_and_checksum_is_verified() {
        let (workspace, _, manifest_hash) = workspace_fixture();
        let external_tools = tempfile::tempdir().unwrap();
        let relative = PathBuf::from(format!("rojo{}", std::env::consts::EXE_SUFFIX));
        let executable = compile_fake_rojo(external_tools.path(), &relative);
        let sha256 = sha256_file(&executable);
        write_lock(workspace.path(), &manifest_hash, &relative, &sha256);
        let adapter = RojoDesktopConfigAdapter::with_trusted_tool_root(external_tools.path())
            .unwrap()
            .with_session_policy(2, 8192)
            .unwrap();
        let resolved = adapter
            .resolve(&RojoDesktopSelection {
                workspace_root: workspace.path().to_path_buf(),
                place_key: None,
            })
            .unwrap();
        assert_eq!(resolved.session_request().max_restarts, 2);
        assert_eq!(resolved.session_request().log_limit_bytes, 8192);

        let platform = current_rojo_platform_key().unwrap();
        let bad_checksum = serde_json::json!({
            "schemaVersion": "1.0",
            "manifestHash": manifest_hash,
            "toolchain": {"rojo": {
                "version": "7.7.0",
                "artifacts": {(platform): {
                    "path": relative,
                    "sha256": format!("sha256:{}", "0".repeat(64))
                }}
            }}
        });
        fs::write(
            workspace.path().join("neuman.lock.json"),
            serde_json::to_vec(&bad_checksum).unwrap(),
        )
        .unwrap();
        assert_eq!(
            adapter
                .resolve(&RojoDesktopSelection {
                    workspace_root: workspace.path().to_path_buf(),
                    place_key: None,
                })
                .unwrap_err()
                .code,
            "GIT_ROJO_VERSION_MISMATCH"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_tool_artifact_symlink_escape() {
        use std::os::unix::fs::symlink;

        let (workspace, tool_root, manifest_hash) = workspace_fixture();
        let outside = tempfile::tempdir().unwrap();
        let outside_executable = compile_fake_rojo(outside.path(), Path::new("rojo"));
        let link = tool_root.join("linked-rojo");
        symlink(&outside_executable, &link).unwrap();
        write_lock(
            workspace.path(),
            &manifest_hash,
            Path::new("linked-rojo"),
            &sha256_file(&outside_executable),
        );
        assert_eq!(
            RojoDesktopConfigAdapter::workspace_scoped()
                .resolve(&RojoDesktopSelection {
                    workspace_root: workspace.path().to_path_buf(),
                    place_key: None,
                })
                .unwrap_err()
                .code,
            "ROJO_LOCK_ARTIFACT_PATH_INVALID"
        );
    }

    #[test]
    fn exact_version_and_sha_parsers_are_canonical() {
        assert!(validate_exact_version("7.7.0").is_ok());
        assert!(validate_exact_version("7.7.0-rc.1+build.4").is_ok());
        assert!(validate_exact_version("07.7.0").is_err());
        assert!(validate_exact_version("7.7").is_err());
        assert!(parse_sha256_value(&Value::String("a".repeat(64))).is_ok());
        assert!(parse_sha256_value(&Value::String(format!("sha256:{}", "f".repeat(64)))).is_ok());
        assert!(parse_sha256_value(&Value::String("A".repeat(64))).is_err());
    }

    #[test]
    fn ownership_preflight_resolves_nested_projects_and_source_paths() {
        let workspace = tempfile::tempdir().unwrap();
        let yaml = starter_manifest("ownership-test", "Ownership Test").unwrap();
        let ownership = ownership_for(workspace.path(), &yaml);
        fs::create_dir_all(workspace.path().join("src/server/modules")).unwrap();
        fs::write(
            workspace.path().join("src/server/modules/Main.luau"),
            b"return {}",
        )
        .unwrap();
        fs::write(
            workspace.path().join("src/server/package.project.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "Package",
                "tree": {
                    "$className": "Folder",
                    "$ignoreUnknownInstances": false,
                    "Modules": {"$path": "modules"}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let project = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Nested",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "ServerScriptService": {
                        "$className": "ServerScriptService",
                        "$ignoreUnknownInstances": false,
                        "Package": {"$path": "src/server/package.project.json"}
                    }
                }
            }),
        );
        let root = fs::canonicalize(workspace.path()).unwrap();
        let report = preflight_rojo_ownership(&root, &project, &ownership).unwrap();
        assert_eq!(report.project_files.len(), 2);
        assert!(report.mappings.iter().any(|mapping| {
            mapping.destination == "/ServerScriptService/Package/Modules"
                && mapping.source == Some(PathBuf::from("src/server/modules"))
                && mapping.owner_id == "server-code"
        }));
    }

    #[test]
    fn ownership_preflight_rejects_escape_dynamic_and_source_mismatch() {
        let workspace = tempfile::tempdir().unwrap();
        let yaml = starter_manifest("hostile-test", "Hostile Test").unwrap();
        let ownership = ownership_for(workspace.path(), &yaml);
        fs::create_dir_all(workspace.path().join("src/server")).unwrap();
        fs::create_dir_all(workspace.path().join("src/other")).unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();

        let escaping = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Escape",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "ServerScriptService": {"$path": "../outside"}
                }
            }),
        );
        assert_eq!(
            preflight_rojo_ownership(&root, &escaping, &ownership)
                .unwrap_err()
                .code,
            "ROJO_PROJECT_PATH_ESCAPE"
        );

        let dynamic = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Dynamic",
                "syncRules": [{"pattern": "*.special", "use": "text"}],
                "tree": {"$className": "DataModel", "$ignoreUnknownInstances": false}
            }),
        );
        assert_eq!(
            preflight_rojo_ownership(&root, &dynamic, &ownership)
                .unwrap_err()
                .code,
            "ROJO_PROJECT_UNSUPPORTED"
        );

        let mismatch = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Mismatch",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "ServerScriptService": {"$path": "src/other"}
                }
            }),
        );
        assert_eq!(
            preflight_rojo_ownership(&root, &mismatch, &ownership)
                .unwrap_err()
                .code,
            "ROJO_OWNERSHIP_SOURCE_MISMATCH"
        );
    }

    #[test]
    fn ownership_preflight_blocks_ancestor_and_artist_mappings() {
        let workspace = tempfile::tempdir().unwrap();
        let yaml = starter_manifest("overlap-test", "Overlap Test").unwrap();
        let ownership = ownership_for(workspace.path(), &yaml);
        fs::create_dir_all(workspace.path().join("src/server")).unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let project = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Overlap",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "Workspace": {
                        "$className": "Workspace",
                        "$path": "src/server"
                    }
                }
            }),
        );
        assert_eq!(
            preflight_rojo_ownership(&root, &project, &ownership)
                .unwrap_err()
                .code,
            "ROJO_OWNERSHIP_CONFLICT"
        );

        let implicit_delete = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Implicit Delete",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "Workspace": {
                        "$className": "Workspace",
                        "Code": {"$path": "src/server"}
                    }
                }
            }),
        );
        assert_eq!(
            preflight_rojo_ownership(&root, &implicit_delete, &ownership)
                .unwrap_err()
                .code,
            "ROJO_OWNERSHIP_CONFLICT"
        );
    }

    #[test]
    fn binary_models_require_an_explicit_git_owner_override() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src/server")).unwrap();
        fs::write(
            workspace.path().join("src/server/Ambiguous.rbxm"),
            b"binary model fixture",
        )
        .unwrap();
        let yaml = starter_manifest("binary-test", "Binary Test").unwrap();
        let ownership = ownership_for(workspace.path(), &yaml);
        let project = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Binary",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "ServerScriptService": {"$path": "src/server"}
                }
            }),
        );
        let root = fs::canonicalize(workspace.path()).unwrap();
        assert_eq!(
            preflight_rojo_ownership(&root, &project, &ownership)
                .unwrap_err()
                .code,
            "ROJO_BINARY_MODEL_OVERRIDE_REQUIRED"
        );

        let approved_yaml = yaml.replace(
            "projectPath: src/server, unknownInstances: reject",
            "projectPath: src/server, unknownInstances: reject, allowRojoBinaryModels: true",
        );
        let approved = ownership_for(workspace.path(), &approved_yaml);
        let report = preflight_rojo_ownership(&root, &project, &approved).unwrap();
        assert!(report.mappings.iter().any(|mapping| mapping.binary_model));
    }

    #[test]
    fn ownership_preflight_rejects_symlinked_source_entries_when_supported() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let yaml = starter_manifest("symlink-test", "Symlink Test").unwrap();
        let ownership = ownership_for(workspace.path(), &yaml);
        fs::create_dir_all(workspace.path().join("src/server")).unwrap();
        let link = workspace.path().join("src/server/escape");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(outside.path(), &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_dir(outside.path(), &link);
        if let Err(error) = link_result {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }
        let project = write_rojo_project(
            workspace.path(),
            &serde_json::json!({
                "name": "Symlink",
                "tree": {
                    "$className": "DataModel",
                    "$ignoreUnknownInstances": false,
                    "ServerScriptService": {"$path": "src/server"}
                }
            }),
        );
        let root = fs::canonicalize(workspace.path()).unwrap();
        assert_eq!(
            preflight_rojo_ownership(&root, &project, &ownership)
                .unwrap_err()
                .code,
            "ROJO_PROJECT_SYMLINK_FORBIDDEN"
        );
    }
}
