//! Fixed Studio runner manifests, authentication, replay protection, and receipts.
//!
//! The runner consumes data only. Repository or Hub content cannot supply Luau
//! source, method names, or arbitrary arguments through this contract.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use url::Url;

use crate::domain::{BuildId, CellId, ContentHash, OperationId, ProjectId, RobloxId, Sha256Hash};

const MANIFEST_DOMAIN: &[u8] = b"neuman-studio-runner-manifest-v1\0";
const RECEIPT_DOMAIN: &[u8] = b"neuman-studio-runner-receipt-v1\0";
const MAX_LIFETIME_MS: i64 = 15 * 60 * 1_000;
const MAX_CLOCK_SKEW_MS: i64 = 60 * 1_000;
const MAX_CELL_BYTES: u64 = 96 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// Native engine surface authorized to consume a runner manifest.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerExecutorProfile {
    /// Signed Studio plugin paired to the local desktop bridge.
    StudioPlugin,
    /// Operator-owned Open Cloud Luau Execution task.
    OperatorOpenCloud,
}

/// Fail-closed runner contract error.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct RunnerError {
    /// Stable error code.
    pub code: &'static str,
    /// User-safe detail.
    pub message: String,
}

impl RunnerError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The only operations understood by the fixed v1 runner.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerOperation {
    /// Insert exact native cells and run declared validators.
    AssembleValidate,
    /// Validate an already assembled candidate without mutation.
    ValidateOnly,
    /// Publish the already validated candidate with `SavePlaceAsync`.
    PublishValidated,
}

/// One exact native art input.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerCellInput {
    /// Stable cell identity.
    pub cell_id: CellId,
    /// Exact managed DataModel slot to replace.
    pub slot: String,
    /// Content-addressed native RBXM bytes.
    pub snapshot_hash: ContentHash,
    /// Exact byte count expected from the transfer.
    pub size_bytes: u64,
    /// Registered native content type.
    pub media_type: String,
}

/// One fixed built-in validator selected by ID/version.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerValidator {
    /// Built-in validator name; never code or a path.
    pub id: String,
    /// Hash of the fixed validator implementation shipped with the runner.
    pub implementation_hash: Sha256Hash,
    /// Whether failure makes the receipt unsuccessful.
    pub blocking: bool,
}

/// Bounded engine-side validation policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerPolicy {
    /// Maximum total instances after assembly.
    pub max_instances: u64,
    /// Maximum aggregate native cell input.
    pub max_native_bytes: u64,
    /// Reject scripts inside art-owned cells.
    pub forbid_art_scripts: bool,
    /// Reject unknown classes/properties under strict compatibility mode.
    pub reject_unknown_schema: bool,
}

/// Profile-specific receipt delivery. Neither variant contains a bearer secret.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "channel", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RunnerReceiptTarget {
    /// Authenticated local callback used only by the Studio-plugin profile.
    Loopback {
        /// Exact numeric-loopback HTTP endpoint.
        endpoint: String,
        /// Non-secret identifier for an out-of-band one-time capability.
        #[serde(rename = "capabilityId")]
        capability_id: String,
    },
    /// Signed task return value retrieved by operator CI from Open Cloud.
    OpenCloudTaskResult {
        /// Non-secret correlation value bound to the operation.
        #[serde(rename = "correlationId")]
        correlation_id: String,
    },
}

/// Declarative input accepted by the fixed Studio runner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerManifest {
    /// Schema version, currently `1`.
    pub schema_version: u16,
    /// Exact engine execution surface; dispatch cannot substitute another one.
    pub executor_profile: RunnerExecutorProfile,
    /// Hash of the fixed built-in Luau runner allowed to execute this manifest.
    pub runner_implementation_hash: Sha256Hash,
    /// Fixed operation selector.
    pub operation: RunnerOperation,
    /// Unique operation identity.
    pub operation_id: OperationId,
    /// Project binding.
    pub project_id: ProjectId,
    /// Build binding.
    pub build_id: BuildId,
    /// Exact logical build identity.
    pub logical_build_hash: ContentHash,
    /// Manifest place key.
    pub place_key: String,
    /// Expected universe for publish operations.
    pub universe_id: Option<RobloxId>,
    /// Expected place for publish operations.
    pub place_id: Option<RobloxId>,
    /// Exact immutable source place version for the Open Cloud profile.
    pub source_place_version_id: Option<RobloxId>,
    /// UTC Unix milliseconds at issue.
    pub issued_at_unix_ms: i64,
    /// UTC Unix milliseconds after which the manifest is invalid.
    pub expires_at_unix_ms: i64,
    /// 256-bit base64url nonce.
    pub nonce: String,
    /// Exact Rojo/base candidate input.
    pub candidate_base_hash: ContentHash,
    /// Ordered native cell inputs.
    pub cells: Vec<RunnerCellInput>,
    /// Ordered built-in validation suite.
    pub validators: Vec<RunnerValidator>,
    /// Resource and content policy.
    pub policy: RunnerPolicy,
    /// Bound result destination.
    pub receipt: RunnerReceiptTarget,
}

impl RunnerManifest {
    /// Validates the complete manifest at `now_unix_ms`.
    pub fn validate(&self, now_unix_ms: i64) -> Result<(), RunnerError> {
        if self.schema_version != 1 {
            return Err(RunnerError::new(
                "RUNNER_SCHEMA_UNSUPPORTED",
                "runner manifest schema must be 1",
            ));
        }
        if self.expires_at_unix_ms <= self.issued_at_unix_ms
            || self.expires_at_unix_ms - self.issued_at_unix_ms > MAX_LIFETIME_MS
        {
            return Err(RunnerError::new(
                "RUNNER_LIFETIME_INVALID",
                "manifest lifetime must be positive and at most 15 minutes",
            ));
        }
        if now_unix_ms < self.issued_at_unix_ms - MAX_CLOCK_SKEW_MS {
            return Err(RunnerError::new(
                "RUNNER_NOT_YET_VALID",
                "manifest issue time is in the future",
            ));
        }
        if now_unix_ms >= self.expires_at_unix_ms {
            return Err(RunnerError::new(
                "RUNNER_MANIFEST_EXPIRED",
                "runner manifest has expired",
            ));
        }
        validate_token("place key", &self.place_key, 1, 64)?;
        let nonce = URL_SAFE_NO_PAD.decode(&self.nonce).map_err(|_| {
            RunnerError::new("RUNNER_NONCE_INVALID", "nonce must be unpadded base64url")
        })?;
        if nonce.len() != 32 {
            return Err(RunnerError::new(
                "RUNNER_NONCE_INVALID",
                "nonce must decode to 256 bits",
            ));
        }
        validate_profile_binding(self)?;
        if self.operation == RunnerOperation::ValidateOnly && !self.cells.is_empty() {
            return Err(RunnerError::new(
                "RUNNER_CELLS_NOT_ALLOWED",
                "validate-only operation cannot insert cells",
            ));
        }

        let mut prior_cell = None::<String>;
        let mut cells = BTreeSet::new();
        let mut slots = BTreeSet::new();
        let mut total = 0_u64;
        for cell in &self.cells {
            let cell_text = cell.cell_id.to_string();
            if prior_cell.as_ref().is_some_and(|prior| prior >= &cell_text) {
                return Err(RunnerError::new(
                    "RUNNER_CELLS_NOT_SORTED",
                    "cells must be strictly ordered by canonical CellId",
                ));
            }
            prior_cell = Some(cell_text);
            if !cells.insert(cell.cell_id) {
                return Err(RunnerError::new(
                    "RUNNER_CELL_DUPLICATE",
                    "cell identity appears more than once",
                ));
            }
            validate_slot(&cell.slot)?;
            if !slots.insert(cell.slot.as_str()) {
                return Err(RunnerError::new(
                    "RUNNER_SLOT_DUPLICATE",
                    "managed slot appears more than once",
                ));
            }
            if cell.size_bytes == 0 || cell.size_bytes > MAX_CELL_BYTES {
                return Err(RunnerError::new(
                    "RUNNER_CELL_SIZE_INVALID",
                    "cell byte size is zero or exceeds 96 MiB",
                ));
            }
            if cell.media_type != "application/vnd.roblox.rbxm" {
                return Err(RunnerError::new(
                    "RUNNER_MEDIA_TYPE_INVALID",
                    "native cells must use application/vnd.roblox.rbxm",
                ));
            }
            total = total.checked_add(cell.size_bytes).ok_or_else(|| {
                RunnerError::new("RUNNER_TOTAL_SIZE_INVALID", "cell byte total overflowed")
            })?;
        }
        if total > MAX_TOTAL_BYTES || total > self.policy.max_native_bytes {
            return Err(RunnerError::new(
                "RUNNER_TOTAL_SIZE_INVALID",
                "native input exceeds the runner or manifest policy limit",
            ));
        }
        if self.policy.max_instances == 0 || self.policy.max_native_bytes > MAX_TOTAL_BYTES {
            return Err(RunnerError::new(
                "RUNNER_POLICY_INVALID",
                "runner policy limits are outside the supported profile",
            ));
        }

        let mut validator_ids = BTreeSet::new();
        for validator in &self.validators {
            validate_token("validator ID", &validator.id, 1, 64)?;
            if !validator_ids.insert(validator.id.as_str()) {
                return Err(RunnerError::new(
                    "RUNNER_VALIDATOR_DUPLICATE",
                    "validator ID appears more than once",
                ));
            }
        }
        Ok(())
    }
}

/// Secret one-operation signing key delivered outside manifest JSON.
pub struct RunnerManifestKey([u8; 32]);

impl RunnerManifestKey {
    /// Creates a key from exactly 256 bits.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Drop for RunnerManifestKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for RunnerManifestKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RunnerManifestKey([REDACTED])")
    }
}

/// Authenticated manifest envelope transferred to the fixed runner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedRunnerManifest {
    /// Declarative manifest.
    pub manifest: RunnerManifest,
    /// `hmac-sha256:<lowercase hex>` over canonical manifest bytes.
    pub signature: String,
}

impl SignedRunnerManifest {
    /// Canonicalizes and authenticates a validated manifest.
    pub fn sign(
        manifest: RunnerManifest,
        key: &RunnerManifestKey,
        now_unix_ms: i64,
    ) -> Result<Self, RunnerError> {
        manifest.validate(now_unix_ms)?;
        let signature = sign_value(MANIFEST_DOMAIN, &manifest, key)?;
        Ok(Self {
            manifest,
            signature,
        })
    }

    /// Verifies signature and complete manifest validity.
    pub fn verify(&self, key: &RunnerManifestKey, now_unix_ms: i64) -> Result<(), RunnerError> {
        verify_value(MANIFEST_DOMAIN, &self.manifest, &self.signature, key)?;
        self.manifest.validate(now_unix_ms)
    }
}

/// Per-daemon replay guard for runner operation IDs and nonces.
#[derive(Default)]
pub struct RunnerReplayGuard {
    operations: HashSet<OperationId>,
    nonces: HashSet<String>,
}

impl RunnerReplayGuard {
    /// Verifies and consumes a manifest exactly once.
    pub fn verify_once(
        &mut self,
        envelope: &SignedRunnerManifest,
        key: &RunnerManifestKey,
        now_unix_ms: i64,
    ) -> Result<(), RunnerError> {
        envelope.verify(key, now_unix_ms)?;
        if self.operations.contains(&envelope.manifest.operation_id)
            || self.nonces.contains(&envelope.manifest.nonce)
        {
            return Err(RunnerError::new(
                "RUNNER_REPLAY_REJECTED",
                "runner operation or nonce was already consumed",
            ));
        }
        self.operations.insert(envelope.manifest.operation_id);
        self.nonces.insert(envelope.manifest.nonce.clone());
        Ok(())
    }
}

/// Result of one built-in validator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerValidatorResult {
    /// Validator ID from the manifest.
    pub id: String,
    /// True only when the fixed validator passed.
    pub passed: bool,
    /// Bounded stable result code.
    pub code: String,
}

/// Fixed runner operation outcome.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerOutcome {
    /// All operation and blocking validation steps succeeded.
    Succeeded,
    /// The runner failed before or during a declared step.
    Failed,
}

/// Open Cloud provider task identity returned after task creation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenCloudTaskEvidence {
    /// Provider execution-session identity.
    pub execution_session_id: String,
    /// Provider task identity.
    pub task_id: String,
    /// Exact source version loaded by the task.
    pub source_place_version_id: RobloxId,
}

/// Publication method proven by a successful publish receipt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerPublicationMethod {
    /// `AssetService:SavePlaceAsync` invoked by the signed Studio plugin.
    StudioSavePlace,
    /// `AssetService:SavePlaceAsync` invoked by operator Open Cloud Luau Execution.
    OpenCloudSavePlace,
}

/// Exact target/version evidence for a successful publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerPublicationEvidence {
    /// Profile-compatible publication mechanism.
    pub method: RunnerPublicationMethod,
    /// Published universe.
    pub universe_id: RobloxId,
    /// Published place.
    pub place_id: RobloxId,
    /// Newly observed place version number.
    pub version_number: RobloxId,
    /// True only if an initially ambiguous provider result was reconciled.
    pub reconciled: bool,
}

/// Structured engine receipt bound to the input manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerReceipt {
    /// Schema version, currently `1`.
    pub schema_version: u16,
    /// Engine surface that produced the receipt.
    pub executor_profile: RunnerExecutorProfile,
    /// Original operation identity.
    pub operation_id: OperationId,
    /// Exact logical build identity.
    pub logical_build_hash: ContentHash,
    /// Hash of the fixed runner source shipped with the application.
    pub runner_hash: Sha256Hash,
    /// Observed Studio version.
    pub studio_version: String,
    /// Operation start time.
    pub started_at_unix_ms: i64,
    /// Operation completion time.
    pub completed_at_unix_ms: i64,
    /// Overall result.
    pub outcome: RunnerOutcome,
    /// Exact observed native snapshots by cell.
    pub applied_cells: BTreeMap<CellId, ContentHash>,
    /// Results for every declared validator.
    pub validators: Vec<RunnerValidatorResult>,
    /// Observed final managed-state root when available.
    pub managed_state_root: Option<ContentHash>,
    /// Required task identity for the Open Cloud profile and absent otherwise.
    pub open_cloud_task: Option<OpenCloudTaskEvidence>,
    /// Required for a successful publish and absent for non-publish operations.
    pub publication: Option<RunnerPublicationEvidence>,
}

impl RunnerReceipt {
    /// Validates this receipt against the immutable manifest.
    pub fn validate_against(&self, manifest: &RunnerManifest) -> Result<(), RunnerError> {
        if self.schema_version != 1
            || self.executor_profile != manifest.executor_profile
            || self.runner_hash != manifest.runner_implementation_hash
            || self.operation_id != manifest.operation_id
            || self.logical_build_hash != manifest.logical_build_hash
        {
            return Err(RunnerError::new(
                "RUNNER_RECEIPT_BINDING_INVALID",
                "receipt does not match manifest identity or fixed runner",
            ));
        }
        if self.studio_version.is_empty()
            || self.studio_version.len() > 128
            || self.completed_at_unix_ms < self.started_at_unix_ms
        {
            return Err(RunnerError::new(
                "RUNNER_RECEIPT_INVALID",
                "receipt version or timing is invalid",
            ));
        }
        let expected_cells = manifest
            .cells
            .iter()
            .map(|cell| (cell.cell_id, cell.snapshot_hash))
            .collect::<BTreeMap<_, _>>();
        let expected_validators = manifest
            .validators
            .iter()
            .map(|validator| validator.id.as_str())
            .collect::<BTreeSet<_>>();
        let observed_validators = self
            .validators
            .iter()
            .map(|result| result.id.as_str())
            .collect::<BTreeSet<_>>();
        if observed_validators.len() != self.validators.len()
            || expected_validators != observed_validators
        {
            return Err(RunnerError::new(
                "RUNNER_RECEIPT_VALIDATORS_INVALID",
                "receipt validator set differs from the manifest",
            ));
        }
        for result in &self.validators {
            validate_token("validator result code", &result.code, 1, 64)?;
        }
        if self.outcome == RunnerOutcome::Succeeded {
            if self.applied_cells != expected_cells {
                return Err(RunnerError::new(
                    "RUNNER_RECEIPT_CELLS_INVALID",
                    "successful receipt does not prove every exact cell",
                ));
            }
            for validator in &manifest.validators {
                let result = self
                    .validators
                    .iter()
                    .find(|result| result.id == validator.id)
                    .expect("validator set was proven equal");
                if validator.blocking && !result.passed {
                    return Err(RunnerError::new(
                        "RUNNER_RECEIPT_OUTCOME_INVALID",
                        "successful receipt contains a failed blocking validator",
                    ));
                }
            }
            if self.managed_state_root.is_none() {
                return Err(RunnerError::new(
                    "RUNNER_RECEIPT_STATE_MISSING",
                    "successful receipt requires a managed state root",
                ));
            }
        }
        match manifest.executor_profile {
            RunnerExecutorProfile::StudioPlugin => {
                if self.open_cloud_task.is_some() {
                    return Err(RunnerError::new(
                        "RUNNER_RECEIPT_PROFILE_INVALID",
                        "Studio-plugin receipt cannot carry Open Cloud task evidence",
                    ));
                }
            }
            RunnerExecutorProfile::OperatorOpenCloud => {
                let evidence = self.open_cloud_task.as_ref().ok_or_else(|| {
                    RunnerError::new(
                        "RUNNER_RECEIPT_PROFILE_INVALID",
                        "Open Cloud receipt requires provider task evidence",
                    )
                })?;
                validate_token(
                    "execution session ID",
                    &evidence.execution_session_id,
                    1,
                    128,
                )?;
                validate_token("task ID", &evidence.task_id, 1, 128)?;
                if Some(&evidence.source_place_version_id)
                    != manifest.source_place_version_id.as_ref()
                {
                    return Err(RunnerError::new(
                        "RUNNER_RECEIPT_SOURCE_VERSION_INVALID",
                        "Open Cloud receipt source version differs from the manifest",
                    ));
                }
            }
        }
        match (&manifest.operation, &self.outcome, &self.publication) {
            (RunnerOperation::PublishValidated, RunnerOutcome::Succeeded, Some(evidence)) => {
                if Some(&evidence.universe_id) != manifest.universe_id.as_ref()
                    || Some(&evidence.place_id) != manifest.place_id.as_ref()
                {
                    return Err(RunnerError::new(
                        "RUNNER_RECEIPT_PUBLICATION_TARGET_INVALID",
                        "publication evidence target differs from the manifest",
                    ));
                }
                let expected_method = match manifest.executor_profile {
                    RunnerExecutorProfile::StudioPlugin => RunnerPublicationMethod::StudioSavePlace,
                    RunnerExecutorProfile::OperatorOpenCloud => {
                        RunnerPublicationMethod::OpenCloudSavePlace
                    }
                };
                if evidence.method != expected_method {
                    return Err(RunnerError::new(
                        "RUNNER_RECEIPT_PUBLICATION_METHOD_INVALID",
                        "publication method is incompatible with the executor profile",
                    ));
                }
            }
            (RunnerOperation::PublishValidated, RunnerOutcome::Succeeded, None) => {
                return Err(RunnerError::new(
                    "RUNNER_RECEIPT_PUBLICATION_MISSING",
                    "successful publish requires exact version evidence",
                ));
            }
            (RunnerOperation::PublishValidated, RunnerOutcome::Failed, _) => {}
            (_, _, None) => {}
            (_, _, Some(_)) => {
                return Err(RunnerError::new(
                    "RUNNER_RECEIPT_PUBLICATION_NOT_ALLOWED",
                    "non-publish receipt cannot carry publication evidence",
                ));
            }
        }
        Ok(())
    }
}

/// Authenticated runner receipt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedRunnerReceipt {
    /// Structured receipt.
    pub receipt: RunnerReceipt,
    /// Canonical HMAC signature.
    pub signature: String,
}

impl SignedRunnerReceipt {
    /// Signs a receipt only after manifest binding validation.
    pub fn sign(
        receipt: RunnerReceipt,
        manifest: &RunnerManifest,
        key: &RunnerManifestKey,
    ) -> Result<Self, RunnerError> {
        receipt.validate_against(manifest)?;
        let signature = sign_value(RECEIPT_DOMAIN, &receipt, key)?;
        Ok(Self { receipt, signature })
    }

    /// Verifies signature and manifest binding.
    pub fn verify(
        &self,
        manifest: &RunnerManifest,
        key: &RunnerManifestKey,
    ) -> Result<(), RunnerError> {
        verify_value(RECEIPT_DOMAIN, &self.receipt, &self.signature, key)?;
        self.receipt.validate_against(manifest)
    }
}

fn validate_token(
    label: &'static str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), RunnerError> {
    if !(minimum..=maximum).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RunnerError::new(
            "RUNNER_TOKEN_INVALID",
            format!("{label} has an invalid shape"),
        ));
    }
    Ok(())
}

fn validate_slot(slot: &str) -> Result<(), RunnerError> {
    if slot.len() > 512
        || !slot.starts_with('/')
        || slot.ends_with('/')
        || slot.contains("//")
        || slot.contains('\\')
        || slot.chars().any(char::is_control)
        || slot
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || matches!(part, "." | "..") || part.len() > 100)
    {
        return Err(RunnerError::new(
            "RUNNER_SLOT_INVALID",
            "cell slot is not a canonical absolute DataModel path",
        ));
    }
    Ok(())
}

fn validate_profile_binding(manifest: &RunnerManifest) -> Result<(), RunnerError> {
    match (&manifest.executor_profile, &manifest.receipt) {
        (
            RunnerExecutorProfile::StudioPlugin,
            RunnerReceiptTarget::Loopback {
                endpoint,
                capability_id,
            },
        ) => {
            validate_token("capability ID", capability_id, 16, 128)?;
            let endpoint = Url::parse(endpoint).map_err(|_| {
                RunnerError::new(
                    "RUNNER_RECEIPT_ENDPOINT_INVALID",
                    "receipt endpoint is not a URL",
                )
            })?;
            let expected_path = format!("/v1/runner/receipts/{}", manifest.operation_id);
            if endpoint.scheme() != "http"
                || endpoint.username() != ""
                || endpoint.password().is_some()
                || endpoint.query().is_some()
                || endpoint.fragment().is_some()
                || endpoint.port().is_none()
                || endpoint.path() != expected_path
                || !matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
            {
                return Err(RunnerError::new(
                    "RUNNER_RECEIPT_ENDPOINT_INVALID",
                    "receipt endpoint must be exact numeric loopback with bound operation path",
                ));
            }
            if manifest.operation == RunnerOperation::PublishValidated {
                require_exact_target(manifest)?;
            } else if manifest.universe_id.is_some() || manifest.place_id.is_some() {
                return Err(RunnerError::new(
                    "RUNNER_TARGET_NOT_ALLOWED",
                    "local non-publish operations cannot carry a Roblox target",
                ));
            }
            if manifest.source_place_version_id.is_some() {
                return Err(RunnerError::new(
                    "RUNNER_SOURCE_VERSION_NOT_ALLOWED",
                    "Studio-plugin jobs cannot claim an Open Cloud source version",
                ));
            }
        }
        (
            RunnerExecutorProfile::OperatorOpenCloud,
            RunnerReceiptTarget::OpenCloudTaskResult { correlation_id },
        ) => {
            if correlation_id != &manifest.operation_id.to_string() {
                return Err(RunnerError::new(
                    "RUNNER_TASK_CORRELATION_INVALID",
                    "task-result correlation must equal the operation identity",
                ));
            }
            require_exact_target(manifest)?;
            if manifest.source_place_version_id.is_none() {
                return Err(RunnerError::new(
                    "RUNNER_SOURCE_VERSION_MISSING",
                    "Open Cloud jobs require an exact immutable source place version",
                ));
            }
        }
        _ => {
            return Err(RunnerError::new(
                "RUNNER_PROFILE_CHANNEL_MISMATCH",
                "receipt channel is incompatible with the executor profile",
            ));
        }
    }
    Ok(())
}

fn require_exact_target(manifest: &RunnerManifest) -> Result<(), RunnerError> {
    if manifest.universe_id.is_none() || manifest.place_id.is_none() {
        return Err(RunnerError::new(
            "RUNNER_PUBLISH_TARGET_MISSING",
            "executor operation requires exact universe and place IDs",
        ));
    }
    Ok(())
}

fn sign_value<T: Serialize>(
    domain: &[u8],
    value: &T,
    key: &RunnerManifestKey,
) -> Result<String, RunnerError> {
    let canonical = serde_jcs::to_vec(value).map_err(|error| {
        RunnerError::new(
            "RUNNER_CANONICALIZATION_FAILED",
            format!("could not canonicalize runner value: {error}"),
        )
    })?;
    let mut message = Vec::with_capacity(domain.len() + canonical.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(&canonical);
    Ok(format!(
        "hmac-sha256:{}",
        hex::encode(hmac_sha256(&key.0, &message))
    ))
}

fn verify_value<T: Serialize>(
    domain: &[u8],
    value: &T,
    signature: &str,
    key: &RunnerManifestKey,
) -> Result<(), RunnerError> {
    let raw = signature.strip_prefix("hmac-sha256:").ok_or_else(|| {
        RunnerError::new(
            "RUNNER_SIGNATURE_INVALID",
            "signature algorithm prefix is invalid",
        )
    })?;
    let supplied = hex::decode(raw).map_err(|_| {
        RunnerError::new(
            "RUNNER_SIGNATURE_INVALID",
            "signature is not lowercase hexadecimal",
        )
    })?;
    if supplied.len() != 32 || raw != hex::encode(&supplied) {
        return Err(RunnerError::new(
            "RUNNER_SIGNATURE_INVALID",
            "signature must be canonical HMAC-SHA-256",
        ));
    }
    let canonical = serde_jcs::to_vec(value).map_err(|error| {
        RunnerError::new(
            "RUNNER_CANONICALIZATION_FAILED",
            format!("could not canonicalize runner value: {error}"),
        )
    })?;
    let mut message = Vec::with_capacity(domain.len() + canonical.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(&canonical);
    let expected = hmac_sha256(&key.0, &message);
    if !bool::from(expected.ct_eq(supplied.as_slice())) {
        return Err(RunnerError::new(
            "RUNNER_SIGNATURE_INVALID",
            "signature verification failed",
        ));
    }
    Ok(())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut normalized = [0_u8; 64];
    normalized[..key.len()].copy_from_slice(key);
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for index in 0..64 {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> RunnerManifest {
        let operation_id = OperationId::new();
        RunnerManifest {
            schema_version: 1,
            executor_profile: RunnerExecutorProfile::StudioPlugin,
            runner_implementation_hash: Sha256Hash::digest(b"fixed runner"),
            operation: RunnerOperation::AssembleValidate,
            operation_id,
            project_id: ProjectId::new(),
            build_id: BuildId::new(),
            logical_build_hash: ContentHash::digest(b"logical"),
            place_key: "lobby".into(),
            universe_id: None,
            place_id: None,
            source_place_version_id: None,
            issued_at_unix_ms: 1_000_000,
            expires_at_unix_ms: 1_300_000,
            nonce: URL_SAFE_NO_PAD.encode([9_u8; 32]),
            candidate_base_hash: ContentHash::digest(b"base"),
            cells: vec![RunnerCellInput {
                cell_id: CellId::new(),
                slot: "/Workspace/Art/Tree".into(),
                snapshot_hash: ContentHash::digest(b"tree"),
                size_bytes: 4,
                media_type: "application/vnd.roblox.rbxm".into(),
            }],
            validators: vec![RunnerValidator {
                id: "ownership-v1".into(),
                implementation_hash: Sha256Hash::digest(b"validator"),
                blocking: true,
            }],
            policy: RunnerPolicy {
                max_instances: 100_000,
                max_native_bytes: 16 * 1024 * 1024,
                forbid_art_scripts: true,
                reject_unknown_schema: true,
            },
            receipt: RunnerReceiptTarget::Loopback {
                endpoint: format!("http://127.0.0.1:43119/v1/runner/receipts/{operation_id}"),
                capability_id: "capability_0123456789abcdef".into(),
            },
        }
    }

    fn successful_receipt(manifest: &RunnerManifest) -> RunnerReceipt {
        RunnerReceipt {
            schema_version: 1,
            executor_profile: manifest.executor_profile,
            operation_id: manifest.operation_id,
            logical_build_hash: manifest.logical_build_hash,
            runner_hash: Sha256Hash::digest(b"fixed runner"),
            studio_version: "0.700.0.7000000".into(),
            started_at_unix_ms: 1_100_000,
            completed_at_unix_ms: 1_110_000,
            outcome: RunnerOutcome::Succeeded,
            applied_cells: manifest
                .cells
                .iter()
                .map(|cell| (cell.cell_id, cell.snapshot_hash))
                .collect(),
            validators: vec![RunnerValidatorResult {
                id: "ownership-v1".into(),
                passed: true,
                code: "ok".into(),
            }],
            managed_state_root: Some(ContentHash::digest(b"state")),
            open_cloud_task: None,
            publication: None,
        }
    }

    #[test]
    fn manifest_signature_detects_tampering_and_wrong_key() {
        let key = RunnerManifestKey::new([7; 32]);
        let mut signed = SignedRunnerManifest::sign(manifest(), &key, 1_100_000).unwrap();
        signed.verify(&key, 1_100_000).unwrap();
        signed.manifest.place_key = "other".into();
        assert_eq!(
            signed.verify(&key, 1_100_000).unwrap_err().code,
            "RUNNER_SIGNATURE_INVALID"
        );
        assert_eq!(
            SignedRunnerManifest::sign(manifest(), &key, 1_100_000)
                .unwrap()
                .verify(&RunnerManifestKey::new([8; 32]), 1_100_000)
                .unwrap_err()
                .code,
            "RUNNER_SIGNATURE_INVALID"
        );
    }

    #[test]
    fn replay_guard_consumes_operation_and_nonce_once() {
        let key = RunnerManifestKey::new([7; 32]);
        let signed = SignedRunnerManifest::sign(manifest(), &key, 1_100_000).unwrap();
        let mut guard = RunnerReplayGuard::default();
        guard.verify_once(&signed, &key, 1_100_000).unwrap();
        assert_eq!(
            guard
                .verify_once(&signed, &key, 1_100_000)
                .unwrap_err()
                .code,
            "RUNNER_REPLAY_REJECTED"
        );
    }

    #[test]
    fn expiry_and_future_issue_are_fail_closed() {
        let key = RunnerManifestKey::new([7; 32]);
        let signed = SignedRunnerManifest::sign(manifest(), &key, 1_100_000).unwrap();
        assert_eq!(
            signed.verify(&key, 1_300_000).unwrap_err().code,
            "RUNNER_MANIFEST_EXPIRED"
        );
        assert_eq!(
            signed.verify(&key, 900_000).unwrap_err().code,
            "RUNNER_NOT_YET_VALID"
        );
    }

    #[test]
    fn receipt_endpoint_rejects_dns_and_wrong_operation() {
        let mut value = manifest();
        value.receipt = RunnerReceiptTarget::Loopback {
            endpoint: format!(
                "http://localhost:43119/v1/runner/receipts/{}",
                value.operation_id
            ),
            capability_id: "capability_0123456789abcdef".into(),
        };
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_RECEIPT_ENDPOINT_INVALID"
        );
        value.receipt = RunnerReceiptTarget::Loopback {
            endpoint: "http://127.0.0.1:43119/v1/runner/receipts/op_wrong".into(),
            capability_id: "capability_0123456789abcdef".into(),
        };
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_RECEIPT_ENDPOINT_INVALID"
        );
    }

    #[test]
    fn binary_inputs_require_unique_canonical_slots_and_limits() {
        let mut value = manifest();
        value.cells[0].slot = "/Workspace//Tree".into();
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_SLOT_INVALID"
        );
        let mut value = manifest();
        value.cells[0].size_bytes = MAX_CELL_BYTES + 1;
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_CELL_SIZE_INVALID"
        );
    }

    #[test]
    fn publish_requires_target_and_other_operations_reject_it() {
        let mut value = manifest();
        value.operation = RunnerOperation::PublishValidated;
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_PUBLISH_TARGET_MISSING"
        );
        value.operation = RunnerOperation::AssembleValidate;
        value.place_id = Some("1".parse().unwrap());
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_TARGET_NOT_ALLOWED"
        );
    }

    #[test]
    fn executor_profile_is_bound_to_its_receipt_channel_and_source() {
        let mut value = manifest();
        value.executor_profile = RunnerExecutorProfile::OperatorOpenCloud;
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_PROFILE_CHANNEL_MISMATCH"
        );

        value.universe_id = Some("100".parse().unwrap());
        value.place_id = Some("200".parse().unwrap());
        value.source_place_version_id = Some("7".parse().unwrap());
        value.receipt = RunnerReceiptTarget::OpenCloudTaskResult {
            correlation_id: value.operation_id.to_string(),
        };
        value.validate(1_100_000).unwrap();

        value.source_place_version_id = None;
        assert_eq!(
            value.validate(1_100_000).unwrap_err().code,
            "RUNNER_SOURCE_VERSION_MISSING"
        );
    }

    #[test]
    fn open_cloud_receipt_requires_exact_provider_task_evidence() {
        let mut value = manifest();
        value.executor_profile = RunnerExecutorProfile::OperatorOpenCloud;
        value.universe_id = Some("100".parse().unwrap());
        value.place_id = Some("200".parse().unwrap());
        value.source_place_version_id = Some("7".parse().unwrap());
        value.receipt = RunnerReceiptTarget::OpenCloudTaskResult {
            correlation_id: value.operation_id.to_string(),
        };
        let mut receipt = successful_receipt(&value);
        assert_eq!(
            receipt.validate_against(&value).unwrap_err().code,
            "RUNNER_RECEIPT_PROFILE_INVALID"
        );
        receipt.open_cloud_task = Some(OpenCloudTaskEvidence {
            execution_session_id: "session-1".into(),
            task_id: "task-1".into(),
            source_place_version_id: "7".parse().unwrap(),
        });
        receipt.validate_against(&value).unwrap();
        receipt
            .open_cloud_task
            .as_mut()
            .unwrap()
            .source_place_version_id = "8".parse().unwrap();
        assert_eq!(
            receipt.validate_against(&value).unwrap_err().code,
            "RUNNER_RECEIPT_SOURCE_VERSION_INVALID"
        );
    }

    #[test]
    fn successful_publish_requires_exact_profile_compatible_version_evidence() {
        let mut value = manifest();
        value.operation = RunnerOperation::PublishValidated;
        value.universe_id = Some("100".parse().unwrap());
        value.place_id = Some("200".parse().unwrap());
        let mut receipt = successful_receipt(&value);
        assert_eq!(
            receipt.validate_against(&value).unwrap_err().code,
            "RUNNER_RECEIPT_PUBLICATION_MISSING"
        );
        receipt.publication = Some(RunnerPublicationEvidence {
            method: RunnerPublicationMethod::StudioSavePlace,
            universe_id: "100".parse().unwrap(),
            place_id: "200".parse().unwrap(),
            version_number: "12".parse().unwrap(),
            reconciled: false,
        });
        receipt.validate_against(&value).unwrap();
        receipt.publication.as_mut().unwrap().method = RunnerPublicationMethod::OpenCloudSavePlace;
        assert_eq!(
            receipt.validate_against(&value).unwrap_err().code,
            "RUNNER_RECEIPT_PUBLICATION_METHOD_INVALID"
        );
    }

    #[test]
    fn signed_success_receipt_proves_exact_cells_and_validators() {
        let manifest = manifest();
        let key = RunnerManifestKey::new([7; 32]);
        let signed =
            SignedRunnerReceipt::sign(successful_receipt(&manifest), &manifest, &key).unwrap();
        signed.verify(&manifest, &key).unwrap();

        let mut missing = successful_receipt(&manifest);
        missing.applied_cells.clear();
        assert_eq!(
            missing.validate_against(&manifest).unwrap_err().code,
            "RUNNER_RECEIPT_CELLS_INVALID"
        );
    }

    #[test]
    fn successful_receipt_cannot_hide_blocking_failure() {
        let manifest = manifest();
        let mut receipt = successful_receipt(&manifest);
        receipt.validators[0].passed = false;
        receipt.validators[0].code = "ownership-failed".into();
        assert_eq!(
            receipt.validate_against(&manifest).unwrap_err().code,
            "RUNNER_RECEIPT_OUTCOME_INVALID"
        );
    }
}
