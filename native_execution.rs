//! Profile-aware orchestration for native Roblox assembly.
//!
//! Jobs and actions are deliberately safe to persist and send through a local
//! control plane. Operator API keys are accepted only by the operator adapter
//! call and are not fields of any serializable type.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{CellId, OperationId, RobloxId, Sha256Hash};
use crate::studio_runner::{
    RunnerExecutorProfile, RunnerManifestKey, SignedRunnerManifest, SignedRunnerReceipt,
};

const MAX_OPEN_CLOUD_TIMEOUT_MS: i64 = 5 * 60 * 1_000;
const MIN_POLL_INTERVAL_MS: i64 = 500;
const MAX_POLL_INTERVAL_MS: i64 = 30 * 1_000;
const MAX_POLL_COUNT: u16 = 600;

/// Fail-closed native-execution orchestration error.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct NativeExecutionError {
    /// Stable machine-readable error code.
    pub code: &'static str,
    /// Secret-free operator detail.
    pub message: String,
}

impl NativeExecutionError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Dispatchable public Studio-plugin job.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudioPluginJob {
    /// Signed declarative runner envelope.
    pub envelope: SignedRunnerManifest,
    /// Exact signed plugin implementation expected to execute the job.
    pub expected_plugin_hash: Sha256Hash,
    /// Paired local Studio session selected by the user/orchestrator.
    pub studio_session_id: String,
}

impl StudioPluginJob {
    /// Creates a job only for a currently valid Studio-plugin manifest.
    pub fn new(
        envelope: SignedRunnerManifest,
        expected_plugin_hash: Sha256Hash,
        observed_plugin_hash: Sha256Hash,
        studio_session_id: String,
        manifest_key: &RunnerManifestKey,
        now_unix_ms: i64,
    ) -> Result<Self, NativeExecutionError> {
        envelope
            .verify(manifest_key, now_unix_ms)
            .map_err(map_runner_error)?;
        if envelope.manifest.executor_profile != RunnerExecutorProfile::StudioPlugin {
            return Err(NativeExecutionError::new(
                "NATIVE_PROFILE_MISMATCH",
                "Studio-plugin job requires a Studio-plugin manifest",
            ));
        }
        if observed_plugin_hash != expected_plugin_hash {
            return Err(NativeExecutionError::new(
                "NATIVE_PLUGIN_IDENTITY_MISMATCH",
                "paired Studio session reported an unexpected plugin implementation",
            ));
        }
        validate_provider_id("Studio session ID", &studio_session_id)?;
        Ok(Self {
            envelope,
            expected_plugin_hash,
            studio_session_id,
        })
    }

    /// Verifies the authenticated receipt returned by the selected plugin.
    pub fn verify_receipt(
        &self,
        receipt: &SignedRunnerReceipt,
        manifest_key: &RunnerManifestKey,
    ) -> Result<(), NativeExecutionError> {
        receipt
            .verify(&self.envelope.manifest, manifest_key)
            .map_err(map_runner_error)
    }
}

/// Immutable exact Open Cloud target loaded by operator CI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenCloudExecutionTarget {
    /// Target universe.
    pub universe_id: RobloxId,
    /// Target place.
    pub place_id: RobloxId,
    /// Immutable source place version loaded for execution.
    pub source_place_version_id: RobloxId,
}

/// Persistable operator-owned Open Cloud job; contains no credential.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorOpenCloudJob {
    /// Signed declarative runner envelope.
    pub envelope: SignedRunnerManifest,
    /// Exact built-in Luau runner source hash owned by the operator adapter.
    pub fixed_runner_hash: Sha256Hash,
    /// Maximum operation duration, capped by the documented provider limit.
    pub timeout_ms: i64,
    /// Poll interval enforced by the durable state machine.
    pub poll_interval_ms: i64,
    /// Bound provider target derived from the signed manifest.
    pub target: OpenCloudExecutionTarget,
}

impl OperatorOpenCloudJob {
    /// Creates a key-free operator job from a valid Open Cloud manifest.
    pub fn new(
        envelope: SignedRunnerManifest,
        fixed_runner_hash: Sha256Hash,
        timeout_ms: i64,
        poll_interval_ms: i64,
        manifest_key: &RunnerManifestKey,
        now_unix_ms: i64,
    ) -> Result<Self, NativeExecutionError> {
        envelope
            .verify(manifest_key, now_unix_ms)
            .map_err(map_runner_error)?;
        if envelope.manifest.executor_profile != RunnerExecutorProfile::OperatorOpenCloud {
            return Err(NativeExecutionError::new(
                "NATIVE_PROFILE_MISMATCH",
                "operator Open Cloud job requires an operator-open-cloud manifest",
            ));
        }
        if envelope.manifest.runner_implementation_hash != fixed_runner_hash {
            return Err(NativeExecutionError::new(
                "NATIVE_RUNNER_IDENTITY_MISMATCH",
                "operator adapter runner differs from the signed manifest",
            ));
        }
        if !(1..=MAX_OPEN_CLOUD_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(NativeExecutionError::new(
                "NATIVE_TIMEOUT_INVALID",
                "Open Cloud timeout must be positive and at most five minutes",
            ));
        }
        if !(MIN_POLL_INTERVAL_MS..=MAX_POLL_INTERVAL_MS).contains(&poll_interval_ms) {
            return Err(NativeExecutionError::new(
                "NATIVE_POLL_INTERVAL_INVALID",
                "poll interval must be between 500 milliseconds and 30 seconds",
            ));
        }
        let manifest = &envelope.manifest;
        let target = OpenCloudExecutionTarget {
            universe_id: manifest.universe_id.clone().ok_or_else(|| {
                NativeExecutionError::new("NATIVE_TARGET_MISSING", "universe ID is absent")
            })?,
            place_id: manifest.place_id.clone().ok_or_else(|| {
                NativeExecutionError::new("NATIVE_TARGET_MISSING", "place ID is absent")
            })?,
            source_place_version_id: manifest.source_place_version_id.clone().ok_or_else(|| {
                NativeExecutionError::new(
                    "NATIVE_TARGET_MISSING",
                    "source place version ID is absent",
                )
            })?,
        };
        Ok(Self {
            envelope,
            fixed_runner_hash,
            timeout_ms,
            poll_interval_ms,
            target,
        })
    }
}

/// Non-secret provider reference for an uploaded binary input.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenCloudBinaryInputRef {
    /// Manifest cell represented by the provider input.
    pub cell_id: CellId,
    /// Provider input identifier, never a URL.
    pub provider_input_id: String,
}

/// Non-secret provider task identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenCloudTaskRef {
    /// Execution-session identifier.
    pub execution_session_id: String,
    /// Task identifier.
    pub task_id: String,
}

/// One idempotency-aware action emitted by the durable operator state machine.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum OpenCloudAction {
    /// Upload one exact content-addressed RBXM input.
    UploadBinaryInput {
        /// Operation correlation/idempotency identity.
        operation_id: OperationId,
        /// Exact target universe.
        universe_id: RobloxId,
        /// Cell metadata including exact hash and size.
        cell: crate::studio_runner::RunnerCellInput,
    },
    /// Create the fixed-runner task once all exact inputs exist.
    CreateTask {
        /// Operation correlation/idempotency identity.
        operation_id: OperationId,
        /// Exact immutable target.
        target: OpenCloudExecutionTarget,
        /// Exact fixed runner implementation expected by the adapter.
        fixed_runner_hash: Sha256Hash,
        /// Signed manifest; contains no credential or arbitrary Luau.
        envelope: Box<SignedRunnerManifest>,
        /// Ordered input references paired with manifest cells.
        inputs: Vec<OpenCloudBinaryInputRef>,
    },
    /// Poll the exact task. No create retry is generated after this transition.
    PollTask {
        /// Operation correlation identity.
        operation_id: OperationId,
        /// Exact immutable target.
        target: OpenCloudExecutionTarget,
        /// Exact provider task.
        task: OpenCloudTaskRef,
    },
}

/// Provider observation supplied to the state machine by an operator adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenCloudObservation {
    /// Binary input upload was committed.
    BinaryInputCreated(OpenCloudBinaryInputRef),
    /// Task creation was committed.
    TaskCreated(OpenCloudTaskRef),
    /// Exact task is queued or running.
    TaskPending,
    /// Fixed runner returned its authenticated receipt.
    TaskCompleted(Box<SignedRunnerReceipt>),
    /// Provider proved the task failed before producing a valid receipt.
    TaskFailed {
        /// Bounded provider-independent failure code.
        code: String,
    },
    /// Mutation response was lost/ambiguous and requires reconciliation.
    MutationResultUnknown,
}

/// Durable high-level state; safe to persist because it contains no secret.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum OpenCloudExecutionStatus {
    /// Inputs remain to upload.
    Uploading,
    /// All inputs exist and task creation is next.
    ReadyToCreate,
    /// Exact task exists and may be polled.
    Polling,
    /// Receipt was authenticated and bound to the manifest.
    Succeeded,
    /// Provider proved failure.
    Failed {
        /// Bounded failure code.
        code: String,
    },
    /// A mutation may have committed; blind retry is prohibited.
    ReconciliationRequired {
        /// Phase with an ambiguous result.
        phase: String,
    },
    /// Deadline or bounded polling was exhausted.
    TimedOut,
}

/// Durable, one-action-at-a-time Open Cloud executor state machine.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorOpenCloudExecution {
    /// Immutable job.
    pub job: OperatorOpenCloudJob,
    /// Current state.
    pub status: OpenCloudExecutionStatus,
    /// Committed inputs by cell.
    pub uploaded_inputs: BTreeMap<CellId, OpenCloudBinaryInputRef>,
    /// Committed task identity, once known.
    pub task: Option<OpenCloudTaskRef>,
    /// Authenticated terminal receipt.
    pub receipt: Option<SignedRunnerReceipt>,
    /// Action durably reserved for dispatch; presence suppresses duplicate emission.
    pub in_flight: Option<OpenCloudAction>,
    started_at_unix_ms: i64,
    next_poll_at_unix_ms: i64,
    poll_count: u16,
}

impl OperatorOpenCloudExecution {
    /// Starts a durable execution from a validated key-free job.
    #[must_use]
    pub fn new(job: OperatorOpenCloudJob, now_unix_ms: i64) -> Self {
        let status = if job.envelope.manifest.cells.is_empty() {
            OpenCloudExecutionStatus::ReadyToCreate
        } else {
            OpenCloudExecutionStatus::Uploading
        };
        Self {
            job,
            status,
            uploaded_inputs: BTreeMap::new(),
            task: None,
            receipt: None,
            in_flight: None,
            started_at_unix_ms: now_unix_ms,
            next_poll_at_unix_ms: now_unix_ms,
            poll_count: 0,
        }
    }

    /// Returns the only currently valid provider action.
    pub fn next_action(&mut self, now_unix_ms: i64) -> Option<OpenCloudAction> {
        if self.in_flight.is_some() {
            return None;
        }
        if now_unix_ms - self.started_at_unix_ms >= self.job.timeout_ms
            || self.poll_count >= MAX_POLL_COUNT
        {
            if matches!(
                self.status,
                OpenCloudExecutionStatus::Uploading
                    | OpenCloudExecutionStatus::ReadyToCreate
                    | OpenCloudExecutionStatus::Polling
            ) {
                self.status = OpenCloudExecutionStatus::TimedOut;
            }
            return None;
        }
        let action = match &self.status {
            OpenCloudExecutionStatus::Uploading => {
                let cell = self
                    .job
                    .envelope
                    .manifest
                    .cells
                    .iter()
                    .find(|cell| !self.uploaded_inputs.contains_key(&cell.cell_id))?
                    .clone();
                Some(OpenCloudAction::UploadBinaryInput {
                    operation_id: self.job.envelope.manifest.operation_id,
                    universe_id: self.job.target.universe_id.clone(),
                    cell,
                })
            }
            OpenCloudExecutionStatus::ReadyToCreate => Some(OpenCloudAction::CreateTask {
                operation_id: self.job.envelope.manifest.operation_id,
                target: self.job.target.clone(),
                fixed_runner_hash: self.job.fixed_runner_hash,
                envelope: Box::new(self.job.envelope.clone()),
                inputs: self.uploaded_inputs.values().cloned().collect(),
            }),
            OpenCloudExecutionStatus::Polling if now_unix_ms >= self.next_poll_at_unix_ms => {
                Some(OpenCloudAction::PollTask {
                    operation_id: self.job.envelope.manifest.operation_id,
                    target: self.job.target.clone(),
                    task: self.task.clone().expect("polling state has exact task"),
                })
            }
            _ => None,
        }?;
        self.in_flight = Some(action.clone());
        Some(action)
    }

    /// Recovers a durably recorded dispatch after process restart.
    ///
    /// Read-only polls can be emitted again. Mutations become ambiguous and
    /// require provider reconciliation before any further action.
    pub fn recover_in_flight_after_restart(&mut self) {
        let Some(action) = self.in_flight.take() else {
            return;
        };
        match action {
            OpenCloudAction::PollTask { .. } => {}
            OpenCloudAction::UploadBinaryInput { .. } => {
                self.status = OpenCloudExecutionStatus::ReconciliationRequired {
                    phase: "upload-binary-input".into(),
                };
            }
            OpenCloudAction::CreateTask { .. } => {
                self.status = OpenCloudExecutionStatus::ReconciliationRequired {
                    phase: "create-task".into(),
                };
            }
        }
    }

    /// Applies one provider observation to the exact currently valid action.
    pub fn apply_observation(
        &mut self,
        action: &OpenCloudAction,
        observation: OpenCloudObservation,
        manifest_key: &RunnerManifestKey,
        now_unix_ms: i64,
    ) -> Result<(), NativeExecutionError> {
        if now_unix_ms - self.started_at_unix_ms >= self.job.timeout_ms {
            self.status = OpenCloudExecutionStatus::TimedOut;
            self.in_flight = None;
            return Err(NativeExecutionError::new(
                "NATIVE_EXECUTION_TIMED_OUT",
                "provider observation arrived after the operation deadline",
            ));
        }
        let expected = self.in_flight.as_ref().ok_or_else(|| {
            NativeExecutionError::new(
                "NATIVE_ACTION_NOT_EXPECTED",
                "execution has no durably reserved provider action",
            )
        })?;
        if expected != action {
            return Err(NativeExecutionError::new(
                "NATIVE_ACTION_MISMATCH",
                "observation does not correspond to the exact expected action",
            ));
        }
        if matches!(observation, OpenCloudObservation::MutationResultUnknown) {
            let phase = match action {
                OpenCloudAction::UploadBinaryInput { .. } => "upload-binary-input",
                OpenCloudAction::CreateTask { .. } => "create-task",
                OpenCloudAction::PollTask { .. } => {
                    return Err(NativeExecutionError::new(
                        "NATIVE_OBSERVATION_INVALID",
                        "a read-only poll cannot have an unknown mutation result",
                    ));
                }
            };
            self.status = OpenCloudExecutionStatus::ReconciliationRequired {
                phase: phase.into(),
            };
            self.in_flight = None;
            return Ok(());
        }
        match (action, observation) {
            (
                OpenCloudAction::UploadBinaryInput { cell, .. },
                OpenCloudObservation::BinaryInputCreated(input),
            ) => {
                validate_provider_id("binary input ID", &input.provider_input_id)?;
                if input.cell_id != cell.cell_id {
                    return Err(NativeExecutionError::new(
                        "NATIVE_INPUT_BINDING_INVALID",
                        "provider input reference is bound to another cell",
                    ));
                }
                self.uploaded_inputs.insert(input.cell_id, input);
                if self.uploaded_inputs.len() == self.job.envelope.manifest.cells.len() {
                    self.status = OpenCloudExecutionStatus::ReadyToCreate;
                }
            }
            (OpenCloudAction::CreateTask { .. }, OpenCloudObservation::TaskCreated(task)) => {
                validate_provider_id("execution session ID", &task.execution_session_id)?;
                validate_provider_id("task ID", &task.task_id)?;
                self.task = Some(task);
                self.status = OpenCloudExecutionStatus::Polling;
                self.next_poll_at_unix_ms = now_unix_ms + self.job.poll_interval_ms;
            }
            (OpenCloudAction::PollTask { .. }, OpenCloudObservation::TaskPending) => {
                self.poll_count = self.poll_count.saturating_add(1);
                self.next_poll_at_unix_ms = now_unix_ms + self.job.poll_interval_ms;
            }
            (OpenCloudAction::PollTask { .. }, OpenCloudObservation::TaskCompleted(receipt)) => {
                receipt
                    .verify(&self.job.envelope.manifest, manifest_key)
                    .map_err(map_runner_error)?;
                let expected_task = self.task.as_ref().expect("polling state has task");
                let evidence = receipt.receipt.open_cloud_task.as_ref().ok_or_else(|| {
                    NativeExecutionError::new(
                        "NATIVE_TASK_EVIDENCE_MISSING",
                        "receipt lacks Open Cloud task evidence",
                    )
                })?;
                if evidence.execution_session_id != expected_task.execution_session_id
                    || evidence.task_id != expected_task.task_id
                {
                    return Err(NativeExecutionError::new(
                        "NATIVE_TASK_EVIDENCE_MISMATCH",
                        "receipt was produced by another provider task",
                    ));
                }
                self.receipt = Some(*receipt);
                self.status = OpenCloudExecutionStatus::Succeeded;
            }
            (OpenCloudAction::PollTask { .. }, OpenCloudObservation::TaskFailed { code }) => {
                validate_provider_id("task failure code", &code)?;
                self.status = OpenCloudExecutionStatus::Failed { code };
            }
            _ => {
                return Err(NativeExecutionError::new(
                    "NATIVE_OBSERVATION_INVALID",
                    "provider observation is invalid for the expected action",
                ));
            }
        }
        self.in_flight = None;
        Ok(())
    }
}

/// Operator API key held only by the operator adapter process.
///
/// This type intentionally implements neither `Clone` nor `Serialize`.
pub struct OperatorApiKey(Vec<u8>);

impl OperatorApiKey {
    /// Imports a key from the operator's CI secret store.
    pub fn new(value: String) -> Result<Self, NativeExecutionError> {
        if value.is_empty()
            || value.len() > 4096
            || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
        {
            return Err(NativeExecutionError::new(
                "NATIVE_OPERATOR_KEY_INVALID",
                "operator API key has an invalid shape",
            ));
        }
        Ok(Self(value.into_bytes()))
    }

    /// Exposes the key only to an in-process operator HTTP adapter.
    #[must_use]
    pub fn expose_to_adapter(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for OperatorApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperatorApiKey([REDACTED])")
    }
}

impl Drop for OperatorApiKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Operator-only adapter boundary. Desktop, plugin, Hub, jobs, and actions do
/// not implement or receive this trait's credential-bearing call.
pub trait OperatorOpenCloudAdapter {
    /// Adapter-specific transport error.
    type Error;

    /// Executes one exact action using an out-of-band operator credential.
    fn execute(
        &mut self,
        credential: &OperatorApiKey,
        action: &OpenCloudAction,
    ) -> Result<OpenCloudObservation, Self::Error>;
}

fn validate_provider_id(label: &str, value: &str) -> Result<(), NativeExecutionError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(NativeExecutionError::new(
            "NATIVE_PROVIDER_ID_INVALID",
            format!("{label} has an invalid shape"),
        ));
    }
    Ok(())
}

fn map_runner_error(error: crate::studio_runner::RunnerError) -> NativeExecutionError {
    NativeExecutionError::new(error.code, error.message)
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::domain::{BuildId, ContentHash, ProjectId};
    use crate::studio_runner::{
        OpenCloudTaskEvidence, RunnerCellInput, RunnerOperation, RunnerOutcome, RunnerPolicy,
        RunnerReceipt, RunnerReceiptTarget, RunnerValidator, RunnerValidatorResult,
    };

    fn cloud_envelope(key: &RunnerManifestKey) -> SignedRunnerManifest {
        let operation_id = OperationId::new();
        let cell_id = CellId::new();
        let manifest = crate::studio_runner::RunnerManifest {
            schema_version: 1,
            executor_profile: RunnerExecutorProfile::OperatorOpenCloud,
            runner_implementation_hash: Sha256Hash::digest(b"fixed runner"),
            operation: RunnerOperation::AssembleValidate,
            operation_id,
            project_id: ProjectId::new(),
            build_id: BuildId::new(),
            logical_build_hash: ContentHash::digest(b"logical"),
            place_key: "lobby".into(),
            universe_id: Some("100".parse().unwrap()),
            place_id: Some("200".parse().unwrap()),
            source_place_version_id: Some("7".parse().unwrap()),
            issued_at_unix_ms: 1_000_000,
            expires_at_unix_ms: 1_300_000,
            nonce: URL_SAFE_NO_PAD.encode([4_u8; 32]),
            candidate_base_hash: ContentHash::digest(b"base"),
            cells: vec![RunnerCellInput {
                cell_id,
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
                max_native_bytes: 1_000_000,
                forbid_art_scripts: true,
                reject_unknown_schema: true,
            },
            receipt: RunnerReceiptTarget::OpenCloudTaskResult {
                correlation_id: operation_id.to_string(),
            },
        };
        SignedRunnerManifest::sign(manifest, key, 1_100_000).unwrap()
    }

    fn job(key: &RunnerManifestKey) -> OperatorOpenCloudJob {
        OperatorOpenCloudJob::new(
            cloud_envelope(key),
            Sha256Hash::digest(b"fixed runner"),
            240_000,
            1_000,
            key,
            1_100_000,
        )
        .unwrap()
    }

    fn plugin_envelope(key: &RunnerManifestKey) -> SignedRunnerManifest {
        let mut manifest = cloud_envelope(key).manifest;
        manifest.executor_profile = RunnerExecutorProfile::StudioPlugin;
        manifest.universe_id = None;
        manifest.place_id = None;
        manifest.source_place_version_id = None;
        manifest.receipt = RunnerReceiptTarget::Loopback {
            endpoint: format!(
                "http://127.0.0.1:43119/v1/runner/receipts/{}",
                manifest.operation_id
            ),
            capability_id: "capability_0123456789abcdef".into(),
        };
        SignedRunnerManifest::sign(manifest, key, 1_100_000).unwrap()
    }

    fn signed_receipt(
        job: &OperatorOpenCloudJob,
        task: &OpenCloudTaskRef,
        key: &RunnerManifestKey,
    ) -> SignedRunnerReceipt {
        let manifest = &job.envelope.manifest;
        let receipt = RunnerReceipt {
            schema_version: 1,
            executor_profile: RunnerExecutorProfile::OperatorOpenCloud,
            operation_id: manifest.operation_id,
            logical_build_hash: manifest.logical_build_hash,
            runner_hash: job.fixed_runner_hash,
            studio_version: "luau-execution-2026".into(),
            started_at_unix_ms: 1_101_000,
            completed_at_unix_ms: 1_102_000,
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
            managed_state_root: Some(ContentHash::digest(b"managed")),
            open_cloud_task: Some(OpenCloudTaskEvidence {
                execution_session_id: task.execution_session_id.clone(),
                task_id: task.task_id.clone(),
                source_place_version_id: "7".parse().unwrap(),
            }),
            publication: None,
        };
        SignedRunnerReceipt::sign(receipt, manifest, key).unwrap()
    }

    #[test]
    fn operator_state_machine_uploads_creates_once_polls_and_verifies_receipt() {
        let key = RunnerManifestKey::new([5; 32]);
        let mut execution = OperatorOpenCloudExecution::new(job(&key), 1_100_000);
        let upload = execution.next_action(1_100_000).unwrap();
        let cell_id = execution.job.envelope.manifest.cells[0].cell_id;
        execution
            .apply_observation(
                &upload,
                OpenCloudObservation::BinaryInputCreated(OpenCloudBinaryInputRef {
                    cell_id,
                    provider_input_id: "input-1".into(),
                }),
                &key,
                1_100_000,
            )
            .unwrap();
        let create = execution.next_action(1_100_001).unwrap();
        assert!(matches!(create, OpenCloudAction::CreateTask { .. }));
        let task = OpenCloudTaskRef {
            execution_session_id: "session-1".into(),
            task_id: "task-1".into(),
        };
        execution
            .apply_observation(
                &create,
                OpenCloudObservation::TaskCreated(task.clone()),
                &key,
                1_100_001,
            )
            .unwrap();
        assert!(execution.next_action(1_100_500).is_none());
        let poll = execution.next_action(1_101_001).unwrap();
        execution
            .apply_observation(
                &poll,
                OpenCloudObservation::TaskCompleted(Box::new(signed_receipt(
                    &execution.job,
                    &task,
                    &key,
                ))),
                &key,
                1_101_001,
            )
            .unwrap();
        assert_eq!(execution.status, OpenCloudExecutionStatus::Succeeded);
        assert!(execution.next_action(1_101_002).is_none());
    }

    #[test]
    fn unknown_create_result_blocks_blind_retry() {
        let key = RunnerManifestKey::new([5; 32]);
        let mut execution = OperatorOpenCloudExecution::new(job(&key), 1_100_000);
        let upload = execution.next_action(1_100_000).unwrap();
        let cell_id = execution.job.envelope.manifest.cells[0].cell_id;
        execution
            .apply_observation(
                &upload,
                OpenCloudObservation::BinaryInputCreated(OpenCloudBinaryInputRef {
                    cell_id,
                    provider_input_id: "input-1".into(),
                }),
                &key,
                1_100_000,
            )
            .unwrap();
        let create = execution.next_action(1_100_001).unwrap();
        execution
            .apply_observation(
                &create,
                OpenCloudObservation::MutationResultUnknown,
                &key,
                1_100_001,
            )
            .unwrap();
        assert!(matches!(
            execution.status,
            OpenCloudExecutionStatus::ReconciliationRequired { .. }
        ));
        assert!(execution.next_action(1_100_002).is_none());
    }

    #[test]
    fn crash_recovery_retries_poll_but_reconciles_mutations() {
        let key = RunnerManifestKey::new([5; 32]);
        let mut execution = OperatorOpenCloudExecution::new(job(&key), 1_100_000);
        let _upload = execution.next_action(1_100_000).unwrap();
        assert!(execution.next_action(1_100_001).is_none());
        execution.recover_in_flight_after_restart();
        assert!(matches!(
            execution.status,
            OpenCloudExecutionStatus::ReconciliationRequired { .. }
        ));
        assert!(execution.next_action(1_100_002).is_none());

        let mut execution = OperatorOpenCloudExecution::new(job(&key), 1_100_000);
        let cell_id = execution.job.envelope.manifest.cells[0].cell_id;
        let upload = execution.next_action(1_100_000).unwrap();
        execution
            .apply_observation(
                &upload,
                OpenCloudObservation::BinaryInputCreated(OpenCloudBinaryInputRef {
                    cell_id,
                    provider_input_id: "input-1".into(),
                }),
                &key,
                1_100_000,
            )
            .unwrap();
        let create = execution.next_action(1_100_001).unwrap();
        execution
            .apply_observation(
                &create,
                OpenCloudObservation::TaskCreated(OpenCloudTaskRef {
                    execution_session_id: "session-1".into(),
                    task_id: "task-1".into(),
                }),
                &key,
                1_100_001,
            )
            .unwrap();
        let poll = execution.next_action(1_101_001).unwrap();
        assert!(matches!(poll, OpenCloudAction::PollTask { .. }));
        execution.recover_in_flight_after_restart();
        assert_eq!(execution.status, OpenCloudExecutionStatus::Polling);
        assert!(matches!(
            execution.next_action(1_101_002),
            Some(OpenCloudAction::PollTask { .. })
        ));
    }

    #[test]
    fn serialized_job_state_and_action_contain_no_operator_key() {
        let runner_key = RunnerManifestKey::new([5; 32]);
        let secret_text = "roblox-operator-secret-never-persist";
        let credential = OperatorApiKey::new(secret_text.into()).unwrap();
        let mut execution = OperatorOpenCloudExecution::new(job(&runner_key), 1_100_000);
        let action = execution.next_action(1_100_000).unwrap();
        let state_json = serde_json::to_string(&execution).unwrap();
        let action_json = serde_json::to_string(&action).unwrap();
        assert!(!state_json.contains(secret_text));
        assert!(!action_json.contains(secret_text));
        assert_eq!(format!("{credential:?}"), "OperatorApiKey([REDACTED])");
        assert_eq!(credential.expose_to_adapter(), secret_text.as_bytes());
    }

    #[test]
    fn profile_substitution_and_task_evidence_mismatch_fail_closed() {
        let key = RunnerManifestKey::new([5; 32]);
        let mut plugin_envelope = cloud_envelope(&key);
        plugin_envelope.manifest.executor_profile = RunnerExecutorProfile::StudioPlugin;
        assert_eq!(
            StudioPluginJob::new(
                plugin_envelope,
                Sha256Hash::digest(b"plugin"),
                Sha256Hash::digest(b"plugin"),
                "studio-1".into(),
                &key,
                1_100_000,
            )
            .unwrap_err()
            .code,
            "RUNNER_SIGNATURE_INVALID"
        );

        let mut execution = OperatorOpenCloudExecution::new(job(&key), 1_100_000);
        let upload = execution.next_action(1_100_000).unwrap();
        let cell_id = execution.job.envelope.manifest.cells[0].cell_id;
        execution
            .apply_observation(
                &upload,
                OpenCloudObservation::BinaryInputCreated(OpenCloudBinaryInputRef {
                    cell_id,
                    provider_input_id: "input-1".into(),
                }),
                &key,
                1_100_000,
            )
            .unwrap();
        let create = execution.next_action(1_100_001).unwrap();
        let task = OpenCloudTaskRef {
            execution_session_id: "session-1".into(),
            task_id: "task-1".into(),
        };
        execution
            .apply_observation(
                &create,
                OpenCloudObservation::TaskCreated(task.clone()),
                &key,
                1_100_001,
            )
            .unwrap();
        let poll = execution.next_action(1_101_001).unwrap();
        let wrong_task = OpenCloudTaskRef {
            task_id: "task-2".into(),
            ..task
        };
        assert_eq!(
            execution
                .apply_observation(
                    &poll,
                    OpenCloudObservation::TaskCompleted(Box::new(signed_receipt(
                        &execution.job,
                        &wrong_task,
                        &key,
                    ))),
                    &key,
                    1_101_001,
                )
                .unwrap_err()
                .code,
            "NATIVE_TASK_EVIDENCE_MISMATCH"
        );
    }

    #[test]
    fn fixed_runner_and_paired_plugin_identity_are_not_substitutable() {
        let key = RunnerManifestKey::new([5; 32]);
        assert_eq!(
            OperatorOpenCloudJob::new(
                cloud_envelope(&key),
                Sha256Hash::digest(b"other runner"),
                240_000,
                1_000,
                &key,
                1_100_000,
            )
            .unwrap_err()
            .code,
            "NATIVE_RUNNER_IDENTITY_MISMATCH"
        );
        assert_eq!(
            StudioPluginJob::new(
                plugin_envelope(&key),
                Sha256Hash::digest(b"expected plugin"),
                Sha256Hash::digest(b"substituted plugin"),
                "studio-1".into(),
                &key,
                1_100_000,
            )
            .unwrap_err()
            .code,
            "NATIVE_PLUGIN_IDENTITY_MISMATCH"
        );
    }
}
