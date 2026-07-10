//! Fail-closed system Git and pinned Rojo orchestration.
//!
//! This module deliberately owns no credentials. Git network operations use the
//! user's existing credential helper or SSH agent, invoke Git without a shell,
//! and never place a remote URL or credential in an argument. Mutating operations
//! require a clean, attached, conflict-free workspace with no Git operation in
//! progress. Build assembly happens in an exact detached worktree.

#![allow(clippy::missing_errors_doc)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Result type for Git/Rojo operations.
pub type Result<T> = std::result::Result<T, IntegrationError>;

/// Stable error with a SPEC-11-compatible machine code.
#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct IntegrationError {
    /// Machine-stable error code.
    pub code: &'static str,
    /// Redacted human-readable message.
    pub message: String,
    /// Bounded and redacted process evidence, when a child process ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Box<CommandDiagnostics>>,
}

impl IntegrationError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: redact_diagnostics(&message.into()),
            diagnostics: None,
        }
    }

    fn command(
        code: &'static str,
        message: impl Into<String>,
        diagnostics: CommandDiagnostics,
    ) -> Self {
        Self {
            code,
            message: redact_diagnostics(&message.into()),
            diagnostics: Some(Box::new(diagnostics)),
        }
    }
}

/// Resource bounds applied to every child command.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessLimits {
    /// Maximum retained bytes for each of stdout and stderr.
    pub max_output_bytes: usize,
    /// Command timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: 256 * 1024,
            timeout_ms: 120_000,
        }
    }
}

/// Bounded process output suitable for receipts and support bundles.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDiagnostics {
    /// Stable operation label; arguments are intentionally not retained.
    pub operation: String,
    /// Numeric exit code, or `None` for a signal/timeout.
    pub exit_code: Option<i32>,
    /// Sanitized UTF-8-lossy stdout.
    pub stdout: String,
    /// Sanitized UTF-8-lossy stderr.
    pub stderr: String,
    /// Whether stdout exceeded the configured retention bound.
    pub stdout_truncated: bool,
    /// Whether stderr exceeded the configured retention bound.
    pub stderr_truncated: bool,
    /// Whether the process exceeded its timeout and was terminated.
    pub timed_out: bool,
}

struct CapturedCommand {
    status: ExitStatus,
    diagnostics: CommandDiagnostics,
}

fn allowed_environment(command: &mut Command) {
    const NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "SystemRoot",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
        "TMPDIR",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "SSH_ASKPASS",
        "GIT_ASKPASS",
        "DISPLAY",
        "GCM_INTERACTIVE",
        "LANG",
        "LC_ALL",
    ];
    command.env_clear();
    for name in NAMES {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
}

fn drain_bounded<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
) -> thread::JoinHandle<io::Result<(Vec<u8>, bool)>> {
    thread::spawn(move || {
        let mut retained = Vec::with_capacity(limit.min(8192));
        let mut truncated = false;
        let mut chunk = [0_u8; 8192];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            let remaining = limit.saturating_sub(retained.len());
            let keep = remaining.min(count);
            retained.extend_from_slice(&chunk[..keep]);
            truncated |= keep != count;
        }
        Ok((retained, truncated))
    })
}

fn run_bounded(
    mut command: Command,
    operation: &str,
    limits: ProcessLimits,
) -> Result<CapturedCommand> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = command.spawn().map_err(|error| {
        IntegrationError::new(
            "PROCESS_START_FAILED",
            format!("could not start {operation}: {error}"),
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| IntegrationError::new("PROCESS_PIPE_FAILED", "stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| IntegrationError::new("PROCESS_PIPE_FAILED", "stderr pipe unavailable"))?;
    let stdout_thread = drain_bounded(stdout, limits.max_output_bytes);
    let stderr_thread = drain_bounded(stderr, limits.max_output_bytes);
    let deadline = Instant::now() + Duration::from_millis(limits.timeout_ms.max(1));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().map_err(|error| {
                    IntegrationError::new("PROCESS_WAIT_FAILED", error.to_string())
                })?;
            }
            Err(error) => {
                return Err(IntegrationError::new(
                    "PROCESS_WAIT_FAILED",
                    error.to_string(),
                ));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_thread
        .join()
        .map_err(|_| IntegrationError::new("PROCESS_PIPE_FAILED", "stdout reader panicked"))?
        .map_err(|error| IntegrationError::new("PROCESS_PIPE_FAILED", error.to_string()))?;
    let (stderr, stderr_truncated) = stderr_thread
        .join()
        .map_err(|_| IntegrationError::new("PROCESS_PIPE_FAILED", "stderr reader panicked"))?
        .map_err(|error| IntegrationError::new("PROCESS_PIPE_FAILED", error.to_string()))?;
    let diagnostics = CommandDiagnostics {
        operation: operation.to_owned(),
        exit_code: status.code(),
        stdout: redact_diagnostics(&String::from_utf8_lossy(&stdout)),
        stderr: redact_diagnostics(&String::from_utf8_lossy(&stderr)),
        stdout_truncated,
        stderr_truncated,
        timed_out,
    };
    if timed_out {
        return Err(IntegrationError::command(
            "PROCESS_TIMEOUT",
            format!("{operation} timed out"),
            diagnostics,
        ));
    }
    Ok(CapturedCommand {
        status,
        diagnostics,
    })
}

fn redact_diagnostics(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("authorization:") || lower.contains("authorization: bearer ") {
            result.push_str("[redacted authorization header]\n");
            continue;
        }
        let mut rest = line;
        while let Some(scheme) = rest.find("://") {
            let (prefix, after_prefix) = rest.split_at(scheme + 3);
            result.push_str(prefix);
            let boundary = after_prefix
                .find(|character: char| character.is_whitespace() || character == '/')
                .unwrap_or(after_prefix.len());
            let authority = &after_prefix[..boundary];
            if let Some(at) = authority.rfind('@') {
                result.push_str("[redacted]@");
                result.push_str(&authority[at + 1..]);
            } else {
                result.push_str(authority);
            }
            rest = &after_prefix[boundary..];
        }
        result.push_str(rest);
        result.push('\n');
    }
    if !input.ends_with('\n') {
        result.pop();
    }
    result
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn output_text(output: &CapturedCommand) -> &str {
    &output.diagnostics.stdout
}

fn ensure_success(
    output: CapturedCommand,
    code: &'static str,
    message: &str,
) -> Result<CapturedCommand> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(IntegrationError::command(code, message, output.diagnostics))
    }
}

fn validate_simple_name(value: &str, field: &'static str) -> Result<()> {
    if value.is_empty()
        || value.len() > 200
        || value.starts_with('-')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
    {
        return Err(IntegrationError::new(
            "GIT_INPUT_INVALID",
            format!("{field} contains unsafe characters"),
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &Path, field: &'static str) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(IntegrationError::new(
            "PATH_UNSAFE",
            format!("{field} must be a normalized relative path"),
        ));
    }
    Ok(())
}

fn canonical_inside(root: &Path, path: &Path, field: &'static str) -> Result<PathBuf> {
    let root = fs::canonicalize(root).map_err(|error| {
        IntegrationError::new("PATH_UNAVAILABLE", format!("{field} root: {error}"))
    })?;
    let path = fs::canonicalize(path)
        .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", format!("{field}: {error}")))?;
    if !path.starts_with(&root) {
        return Err(IntegrationError::new(
            "PATH_UNSAFE",
            format!("{field} escapes its allowed root"),
        ));
    }
    Ok(path)
}

fn command_path(path: &Path) -> OsString {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return OsString::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return OsString::from(rest);
        }
    }
    path.as_os_str().to_owned()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|error| IntegrationError::new("FILE_READ_FAILED", error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024].into_boxed_slice();
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| IntegrationError::new("FILE_READ_FAILED", error.to_string()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Exact Git object format reported by the repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitObjectFormat {
    /// Forty-hex SHA-1 object identifiers.
    Sha1,
    /// Sixty-four-hex SHA-256 object identifiers.
    Sha256,
}

impl GitObjectFormat {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            other => Err(IntegrationError::new(
                "GIT_OBJECT_FORMAT_UNSUPPORTED",
                format!("unsupported object format {other}"),
            )),
        }
    }

    fn oid_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

/// A validated, exact commit identifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ExactCommit(String);

impl<'de> Deserialize<'de> for ExactCommit {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let valid_length = matches!(value.len(), 40 | 64);
        let valid_hex = value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        if !valid_length || !valid_hex {
            return Err(serde::de::Error::custom(
                "exact commit must be 40 or 64 lowercase hexadecimal characters",
            ));
        }
        Ok(Self(value))
    }
}

impl ExactCommit {
    /// Parse a lowercase full-length object ID for the repository format.
    pub fn parse(value: &str, format: GitObjectFormat) -> Result<Self> {
        if value.len() != format.oid_len()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(IntegrationError::new(
                "GIT_OID_INVALID",
                "commit must be a full lowercase hexadecimal object ID",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Return the full object ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An in-progress Git operation that `NeuMan` will not clean up or abort.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitOperation {
    /// Merge in progress.
    Merge,
    /// Rebase in progress.
    Rebase,
    /// Cherry-pick in progress.
    CherryPick,
    /// Revert in progress.
    Revert,
    /// Bisect in progress.
    Bisect,
}

/// Detailed fail-closed workspace observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInspection {
    /// Exact current HEAD.
    pub head: ExactCommit,
    /// Object format used by this repository.
    pub object_format: GitObjectFormat,
    /// Attached branch, or `None` for detached HEAD.
    pub branch: Option<String>,
    /// Configured upstream when available.
    pub upstream: Option<String>,
    /// Commits ahead of upstream.
    pub ahead: u64,
    /// Commits behind upstream.
    pub behind: u64,
    /// Count of staged paths.
    pub staged: usize,
    /// Count of unstaged paths.
    pub unstaged: usize,
    /// Count of untracked paths.
    pub untracked: usize,
    /// Count of conflicted paths.
    pub conflicted: usize,
    /// Active Git operation, if any.
    pub operation: Option<GitOperation>,
    /// Whether sparse checkout is enabled.
    pub sparse_checkout: bool,
    /// Whether the repository is a partial clone.
    pub partial_clone: bool,
}

impl WorkspaceInspection {
    /// True only when no tracked or untracked changes exist.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.unstaged == 0 && self.untracked == 0 && self.conflicted == 0
    }

    /// Enforce the precondition for an update mutation.
    pub fn require_safe_update(&self) -> Result<()> {
        if self.operation.is_some() {
            return Err(IntegrationError::new(
                "GIT_OPERATION_IN_PROGRESS",
                "an existing Git operation must be completed or aborted by the user",
            ));
        }
        if self.conflicted != 0 {
            return Err(IntegrationError::new(
                "GIT_CONFLICT",
                "conflicted paths must be resolved by the user",
            ));
        }
        if !self.is_clean() {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_DIRTY",
                "staged, unstaged, or untracked changes block update",
            ));
        }
        if self.branch.is_none() {
            return Err(IntegrationError::new(
                "GIT_DETACHED_HEAD",
                "detached HEAD is read-only for normal update",
            ));
        }
        Ok(())
    }
}

/// Startup compatibility information for system Git.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitProbe {
    /// Raw bounded `git --version` output.
    pub version: String,
    /// Repository object format.
    pub object_format: GitObjectFormat,
    /// Absolute repository root.
    pub repository_root: PathBuf,
    /// Whether Git LFS is installed.
    pub lfs_version: Option<String>,
}

/// Safe system-Git client rooted at one repository.
#[derive(Clone, Debug)]
pub struct GitClient {
    executable: PathBuf,
    root: PathBuf,
    limits: ProcessLimits,
}

impl GitClient {
    /// Open a repository using `git` from the allowlisted `PATH`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_executable(root, PathBuf::from("git"))
    }

    /// Open with an explicit executable. Useful for packaged Git and tests.
    pub fn open_with_executable(root: impl AsRef<Path>, executable: PathBuf) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref()).map_err(|error| {
            IntegrationError::new("GIT_REPOSITORY_UNAVAILABLE", error.to_string())
        })?;
        let client = Self {
            executable,
            root,
            limits: ProcessLimits::default(),
        };
        let actual = client.git_success(
            &["rev-parse", "--show-toplevel"],
            "git repository discovery",
            "GIT_REPOSITORY_UNTRUSTED",
        )?;
        let actual = fs::canonicalize(output_text(&actual).trim()).map_err(|error| {
            IntegrationError::new("GIT_REPOSITORY_UNAVAILABLE", error.to_string())
        })?;
        if actual != client.root {
            return Err(IntegrationError::new(
                "GIT_REPOSITORY_UNTRUSTED",
                "the supplied path is not the repository top level",
            ));
        }
        Ok(client)
    }

    /// Override child resource limits.
    #[must_use]
    pub fn with_limits(mut self, limits: ProcessLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Absolute repository root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn git_command<I, S>(&self, args: I, cwd: &Path) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.executable);
        command.current_dir(cwd);
        allowed_environment(&mut command);
        command.arg("-c").arg(if cfg!(windows) {
            "core.hooksPath=NUL"
        } else {
            "core.hooksPath=/dev/null"
        });
        command.arg("-c").arg("core.fsmonitor=false");
        // Repository configuration is untrusted input. Even when a caller
        // selects a remote by name, Git can otherwise dispatch `ext::` or an
        // arbitrary remote helper enabled by local configuration.
        command.arg("-c").arg("protocol.ext.allow=never");
        command.arg("-c").arg("fetch.recurseSubmodules=false");
        command.arg("-c").arg("submodule.recurse=false");
        command.args(args);
        command
    }

    fn git(&self, args: &[&str], operation: &str) -> Result<CapturedCommand> {
        run_bounded(self.git_command(args, &self.root), operation, self.limits)
    }

    fn git_success(
        &self,
        args: &[&str],
        operation: &str,
        code: &'static str,
    ) -> Result<CapturedCommand> {
        ensure_success(
            self.git(args, operation)?,
            code,
            &format!("{operation} failed"),
        )
    }

    /// Probe Git, object format, worktree support, and optional LFS.
    pub fn probe(&self) -> Result<GitProbe> {
        let version = self.git_success(
            &["--version"],
            "git version probe",
            "GIT_VERSION_UNSUPPORTED",
        )?;
        let version = output_text(&version).trim().to_owned();
        if !version.starts_with("git version ") {
            return Err(IntegrationError::new(
                "GIT_VERSION_UNSUPPORTED",
                "unexpected Git version response",
            ));
        }
        let format = self.object_format()?;
        self.git_success(
            &["worktree", "list", "--porcelain"],
            "git worktree probe",
            "GIT_VERSION_UNSUPPORTED",
        )?;
        let lfs_version = self
            .git(&["lfs", "version"], "git lfs probe")
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.diagnostics.stdout.trim().to_owned());
        Ok(GitProbe {
            version,
            object_format: format,
            repository_root: self.root.clone(),
            lfs_version,
        })
    }

    /// Determine this repository's object format.
    pub fn object_format(&self) -> Result<GitObjectFormat> {
        let output = self.git_success(
            &["rev-parse", "--show-object-format"],
            "git object format",
            "GIT_VERSION_UNSUPPORTED",
        )?;
        GitObjectFormat::parse(output_text(&output))
    }

    /// Inspect the workspace without changing it.
    pub fn inspect(&self) -> Result<WorkspaceInspection> {
        let format = self.object_format()?;
        let output = self.git_success(
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
                "-z",
            ],
            "git status",
            "GIT_STATUS_FAILED",
        )?;
        let fields: Vec<&str> = output_text(&output)
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .collect();
        let mut head = None;
        let mut branch = None;
        let mut upstream = None;
        let mut ahead = 0;
        let mut behind = 0;
        let mut staged = 0;
        let mut unstaged = 0;
        let mut untracked = 0;
        let mut conflicted = 0;
        for field in fields {
            if let Some(value) = field.strip_prefix("# branch.oid ") {
                head = Some(ExactCommit::parse(value.trim(), format)?);
            } else if let Some(value) = field.strip_prefix("# branch.head ") {
                if value != "(detached)" {
                    branch = Some(value.to_owned());
                }
            } else if let Some(value) = field.strip_prefix("# branch.upstream ") {
                upstream = Some(value.to_owned());
            } else if let Some(value) = field.strip_prefix("# branch.ab ") {
                for part in value.split_whitespace() {
                    if let Some(value) = part.strip_prefix('+') {
                        ahead = value.parse().unwrap_or(0);
                    }
                    if let Some(value) = part.strip_prefix('-') {
                        behind = value.parse().unwrap_or(0);
                    }
                }
            } else if field.starts_with("? ") {
                untracked += 1;
            } else if field.starts_with("u ") {
                conflicted += 1;
            } else if field.starts_with("1 ") || field.starts_with("2 ") {
                let xy = field.as_bytes().get(2..4).unwrap_or_default();
                if xy.first().is_some_and(|value| *value != b'.') {
                    staged += 1;
                }
                if xy.get(1).is_some_and(|value| *value != b'.') {
                    unstaged += 1;
                }
            }
        }
        let head = head.ok_or_else(|| {
            IntegrationError::new("GIT_HEAD_UNBORN", "repository has no committed HEAD")
        })?;
        let operation = self.operation_state()?;
        let sparse_checkout = self
            .git(
                &["config", "--bool", "core.sparseCheckout"],
                "git sparse checkout probe",
            )
            .ok()
            .is_some_and(|output| output.status.success() && output_text(&output).trim() == "true");
        let partial_clone = self
            .git(
                &["config", "--get", "extensions.partialClone"],
                "git partial clone probe",
            )
            .ok()
            .is_some_and(|output| {
                output.status.success() && !output_text(&output).trim().is_empty()
            });
        Ok(WorkspaceInspection {
            head,
            object_format: format,
            branch,
            upstream,
            ahead,
            behind,
            staged,
            unstaged,
            untracked,
            conflicted,
            operation,
            sparse_checkout,
            partial_clone,
        })
    }

    fn operation_state(&self) -> Result<Option<GitOperation>> {
        let checks = [
            ("MERGE_HEAD", GitOperation::Merge),
            ("rebase-merge", GitOperation::Rebase),
            ("rebase-apply", GitOperation::Rebase),
            ("CHERRY_PICK_HEAD", GitOperation::CherryPick),
            ("REVERT_HEAD", GitOperation::Revert),
            ("BISECT_LOG", GitOperation::Bisect),
        ];
        for (name, operation) in checks {
            let output = self.git_success(
                &["rev-parse", "--git-path", name],
                "git operation probe",
                "GIT_STATUS_FAILED",
            )?;
            let value = output_text(&output).trim();
            let path = if Path::new(value).is_absolute() {
                PathBuf::from(value)
            } else {
                self.root.join(value)
            };
            if path.exists() {
                return Ok(Some(operation));
            }
        }
        Ok(None)
    }

    /// Prove that a full object ID resolves to that exact commit object.
    pub fn validate_exact_commit(&self, oid: &str) -> Result<ExactCommit> {
        let exact = ExactCommit::parse(oid, self.object_format()?)?;
        let expression = format!("{}^{{commit}}", exact.as_str());
        let output = self.git_success(
            &["rev-parse", "--verify", &expression],
            "git commit validation",
            "GIT_OID_INVALID",
        )?;
        if output_text(&output).trim() != exact.as_str() {
            return Err(IntegrationError::new(
                "GIT_OID_INVALID",
                "object does not identify the exact commit requested",
            ));
        }
        Ok(exact)
    }

    fn configured_remote(&self, remote: &str) -> Result<Vec<String>> {
        validate_simple_name(remote, "remote")?;
        self.reject_dangerous_local_config()?;
        let output = self.git_success(
            &["remote", "get-url", "--all", remote],
            "git remote validation",
            "GIT_REMOTE_INVALID",
        )?;
        let urls: Vec<String> = output_text(&output)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        if urls.is_empty() {
            return Err(IntegrationError::new(
                "GIT_REMOTE_INVALID",
                "remote has no URL",
            ));
        }
        for url in &urls {
            validate_remote_url(url)?;
        }
        Ok(urls)
    }

    fn reject_dangerous_local_config(&self) -> Result<()> {
        let output = self.git(
            &[
                "config",
                "--local",
                "--includes",
                "--name-only",
                "--get-regexp",
                ".*",
            ],
            "git local security configuration inspection",
        )?;
        if !output.status.success() {
            // `git config --get-regexp` uses exit 1 for no matching values.
            if output.status.code() == Some(1) {
                return Ok(());
            }
            return Err(IntegrationError::command(
                "GIT_REPOSITORY_UNTRUSTED",
                "repository-local Git security configuration could not be inspected",
                output.diagnostics,
            ));
        }
        if output.diagnostics.stdout_truncated {
            return Err(IntegrationError::new(
                "GIT_REPOSITORY_UNTRUSTED",
                "repository-local Git configuration exceeded the inspection bound",
            ));
        }
        if output_text(&output)
            .lines()
            .map(str::trim)
            .any(dangerous_local_git_key)
        {
            return Err(IntegrationError::new(
                "GIT_REPOSITORY_UNTRUSTED",
                "repository-local Git configuration contains an executable or transport-changing setting",
            ));
        }
        Ok(())
    }

    fn remote_refs(&self, remote: &str) -> Result<BTreeMap<String, String>> {
        let prefix = format!("refs/remotes/{remote}/");
        let output = self.git_success(
            &["for-each-ref", "--format=%(refname) %(objectname)", &prefix],
            "git remote ref inspection",
            "GIT_STATUS_FAILED",
        )?;
        Ok(output_text(&output)
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(name, oid)| (name.to_owned(), oid.to_owned()))
            .collect())
    }

    fn require_filter_free_checkout(&self, commit: &ExactCommit) -> Result<()> {
        let listing = self.git_success(
            &["ls-tree", "-r", "--name-only", commit.as_str()],
            "git attribute file inspection",
            "GIT_REPOSITORY_UNTRUSTED",
        )?;
        if listing.diagnostics.stdout_truncated {
            return Err(IntegrationError::new(
                "GIT_REPOSITORY_UNTRUSTED",
                "repository path listing exceeded the inspection bound",
            ));
        }
        for path in output_text(&listing).lines().filter(|path| {
            Path::new(path).file_name().and_then(OsStr::to_str) == Some(".gitattributes")
        }) {
            let object = format!("{}:{path}", commit.as_str());
            let attributes = self.git_success(
                &["show", &object],
                "git attribute content inspection",
                "GIT_REPOSITORY_UNTRUSTED",
            )?;
            if attributes.diagnostics.stdout_truncated
                || attributes_enable_filter(output_text(&attributes))
            {
                return Err(IntegrationError::new(
                    "GIT_REPOSITORY_UNTRUSTED",
                    "checkout filters are disabled for isolated builds; hydrate and verify required LFS objects in a separately qualified provider step",
                ));
            }
        }
        let info_attributes = self.git_success(
            &["rev-parse", "--git-path", "info/attributes"],
            "git local attribute path inspection",
            "GIT_REPOSITORY_UNTRUSTED",
        )?;
        let info_path = PathBuf::from(output_text(&info_attributes).trim());
        let info_path = if info_path.is_absolute() {
            info_path
        } else {
            self.root.join(info_path)
        };
        if info_path.exists() {
            let bytes = fs::read(&info_path).map_err(|error| {
                IntegrationError::new("GIT_REPOSITORY_UNTRUSTED", error.to_string())
            })?;
            if bytes.len() > self.limits.max_output_bytes
                || attributes_enable_filter(&String::from_utf8_lossy(&bytes))
            {
                return Err(IntegrationError::new(
                    "GIT_REPOSITORY_UNTRUSTED",
                    "local Git attributes contain or may hide an executable checkout filter",
                ));
            }
        }
        let global = self.git(
            &["config", "--path", "--get", "core.attributesFile"],
            "git global attribute path inspection",
        )?;
        if global.status.success() {
            let path = PathBuf::from(output_text(&global).trim());
            if path.exists() {
                let bytes = fs::read(path).map_err(|error| {
                    IntegrationError::new("GIT_REPOSITORY_UNTRUSTED", error.to_string())
                })?;
                if bytes.len() > self.limits.max_output_bytes
                    || attributes_enable_filter(&String::from_utf8_lossy(&bytes))
                {
                    return Err(IntegrationError::new(
                        "GIT_REPOSITORY_UNTRUSTED",
                        "global Git attributes contain or may hide an executable checkout filter",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Fetch a configured remote by name. No remote URL is passed or retained.
    pub fn fetch(&self, remote: &str, options: FetchOptions) -> Result<FetchReceipt> {
        self.configured_remote(remote)?;
        let before = self.remote_refs(remote)?;
        let mut args = vec!["fetch", "--no-recurse-submodules"];
        if options.prune {
            args.push("--prune");
        }
        match options.tags {
            TagPolicy::Auto => {}
            TagPolicy::All => args.push("--tags"),
            TagPolicy::None => args.push("--no-tags"),
        }
        args.push("--");
        args.push(remote);
        let output = self.git(&args, "git fetch")?;
        let diagnostics = output.diagnostics.clone();
        ensure_success(
            output,
            "GIT_FETCH_FAILED",
            "fetch failed; repository state was preserved",
        )?;
        let after = self.remote_refs(remote)?;
        let changes = changed_refs(&before, &after);
        Ok(FetchReceipt {
            remote: remote.to_owned(),
            started_from: before,
            finished_at_epoch_ms: epoch_millis(),
            changed_refs: changes,
            diagnostics,
        })
    }

    /// Update the attached branch by fast-forward only after a second safe-state check.
    pub fn update_fast_forward(&self, upstream: &str) -> Result<UpdateReceipt> {
        validate_simple_name(upstream, "upstream")?;
        self.reject_dangerous_local_config()?;
        let before_state = self.inspect()?;
        before_state.require_safe_update()?;
        let before = before_state.head.clone();
        let target = self.resolve_commit(upstream)?;
        self.require_filter_free_checkout(&target)?;
        let output = self.git(
            &["merge", "--ff-only", "--no-edit", "--", upstream],
            "git fast-forward update",
        )?;
        if !output.status.success() {
            let state = self.inspect()?;
            let code = if state.conflicted != 0 {
                "GIT_CONFLICT"
            } else {
                "GIT_UPDATE_NOT_FAST_FORWARD"
            };
            return Err(IntegrationError::command(
                code,
                "fast-forward update failed; no conflict resolution was attempted",
                output.diagnostics,
            ));
        }
        let after_state = self.inspect()?;
        if after_state.operation.is_some() || after_state.conflicted != 0 {
            return Err(IntegrationError::new(
                "GIT_OPERATION_IN_PROGRESS",
                "Git reported success but left an incomplete operation",
            ));
        }
        Ok(UpdateReceipt {
            before,
            after: after_state.head,
            branch: after_state.branch.ok_or_else(|| {
                IntegrationError::new("GIT_DETACHED_HEAD", "branch detached during update")
            })?,
            upstream: upstream.to_owned(),
            completed_at_epoch_ms: epoch_millis(),
            diagnostics: output.diagnostics,
        })
    }

    fn resolve_commit(&self, reference: &str) -> Result<ExactCommit> {
        validate_simple_name(reference, "commit reference")?;
        let expression = format!("{reference}^{{commit}}");
        let output = self.git_success(
            &["rev-parse", "--verify", &expression],
            "git commit reference resolution",
            "GIT_OID_INVALID",
        )?;
        ExactCommit::parse(output_text(&output).trim(), self.object_format()?)
    }

    /// Create a clean detached worktree at an exact commit beneath a dedicated cache root.
    pub fn create_build_worktree(
        &self,
        commit: &ExactCommit,
        cache_root: &Path,
        name: &str,
    ) -> Result<BuildWorktreeReceipt> {
        validate_simple_name(name, "worktree name")?;
        if name.contains('/') {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "worktree name must be one path component",
            ));
        }
        self.validate_exact_commit(commit.as_str())?;
        self.require_filter_free_checkout(commit)?;
        fs::create_dir_all(cache_root).map_err(|error| {
            IntegrationError::new("GIT_WORKTREE_CREATE_FAILED", error.to_string())
        })?;
        let cache_root = fs::canonicalize(cache_root).map_err(|error| {
            IntegrationError::new("GIT_WORKTREE_CREATE_FAILED", error.to_string())
        })?;
        if cache_root.starts_with(&self.root) || self.root.starts_with(&cache_root) {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "build cache must not contain or be contained by the source repository",
            ));
        }
        let path = cache_root.join(name);
        if path.exists() {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_CREATE_FAILED",
                "worktree target already exists",
            ));
        }
        let path_arg = command_path(&path);
        let args = [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            OsString::from("--"),
            path_arg,
            OsString::from(commit.as_str()),
        ];
        let output = run_bounded(
            self.git_command(&args, &self.root),
            "git detached worktree create",
            self.limits,
        )?;
        ensure_success(
            output,
            "GIT_WORKTREE_CREATE_FAILED",
            "could not create detached worktree",
        )?;
        let actual = fs::canonicalize(&path).map_err(|error| {
            IntegrationError::new("GIT_WORKTREE_CREATE_FAILED", error.to_string())
        })?;
        if actual.parent() != Some(cache_root.as_path()) {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "created worktree escaped cache root",
            ));
        }
        let receipt = BuildWorktreeReceipt {
            repository_root: self.root.clone(),
            cache_root,
            path: actual.clone(),
            commit: commit.clone(),
            created_at_epoch_ms: epoch_millis(),
            state: WorktreeReceiptState::Active,
        };
        write_worktree_receipt(&receipt)?;
        let worktree_client = GitClient {
            executable: self.executable.clone(),
            root: actual.clone(),
            limits: self.limits,
        };
        let state = worktree_client.inspect()?;
        if state.head != *commit || !state.is_clean() || state.operation.is_some() {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_CREATE_FAILED",
                "created worktree did not match the exact clean commit",
            ));
        }
        Ok(receipt)
    }

    /// Remove an owned detached worktree only after immutable artifact persistence.
    pub fn cleanup_build_worktree(
        &self,
        receipt: &BuildWorktreeReceipt,
        artifact_persisted: bool,
    ) -> Result<WorktreeCleanupReceipt> {
        if !artifact_persisted {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_CLEANUP_BLOCKED",
                "artifact receipt must be persisted before worktree cleanup",
            ));
        }
        receipt.validate_for(self)?;
        let args = [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--"),
            command_path(&receipt.path),
        ];
        let output = run_bounded(
            self.git_command(&args, &self.root),
            "git worktree cleanup",
            self.limits,
        )?;
        let diagnostics = output.diagnostics.clone();
        ensure_success(
            output,
            "GIT_WORKTREE_CLEANUP_FAILED",
            "could not remove owned build worktree",
        )?;
        self.git_success(
            &["worktree", "prune", "--expire", "now"],
            "git worktree metadata prune",
            "GIT_WORKTREE_CLEANUP_FAILED",
        )?;
        let cleanup = WorktreeCleanupReceipt {
            path: receipt.path.clone(),
            commit: receipt.commit.clone(),
            artifact_persisted,
            removed: !receipt.path.exists(),
            completed_at_epoch_ms: epoch_millis(),
            diagnostics,
        };
        write_cleanup_receipt(receipt, &cleanup)?;
        Ok(cleanup)
    }

    /// Reconcile recorded build worktrees without deleting anything.
    pub fn reconcile_build_cache(&self, cache_root: &Path) -> Result<Vec<WorktreeReconciliation>> {
        let cache_root = fs::canonicalize(cache_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        let output = self.git_success(
            &["worktree", "list", "--porcelain"],
            "git worktree reconciliation",
            "GIT_STATUS_FAILED",
        )?;
        let registered: Vec<PathBuf> = output_text(&output)
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect();
        let mut reconciled = Vec::new();
        for entry in fs::read_dir(&cache_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?
        {
            let entry = entry
                .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(OsStr::to_str) != Some("json")
                || !path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.ends_with(".neuman-worktree.json"))
            {
                continue;
            }
            let bytes = fs::read(&path).map_err(|error| {
                IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string())
            })?;
            let receipt: BuildWorktreeReceipt =
                serde_json::from_slice(&bytes).map_err(|error| {
                    IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string())
                })?;
            let exists = receipt.path.exists();
            let is_registered = fs::canonicalize(&receipt.path)
                .ok()
                .is_some_and(|candidate| registered.contains(&candidate));
            reconciled.push(WorktreeReconciliation {
                receipt,
                exists,
                registered: is_registered,
                requires_operator_review: exists != is_registered,
            });
        }
        Ok(reconciled)
    }
}

fn dangerous_local_git_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "core.sshcommand"
        || key == "core.gitproxy"
        || key == "core.askpass"
        || key == "core.attributesfile"
        || key == "core.hookspath"
        || key == "core.fsmonitor"
        || key == "credential.helper"
        || key.starts_with("credential.")
        || key.starts_with("filter.")
        || key.starts_with("http.")
        || key.starts_with("https.")
        || key.starts_with("protocol.")
        || key.starts_with("url.")
        || key.starts_with("include.")
        || key.starts_with("includeif.")
        || (key.starts_with("remote.")
            && [".uploadpack", ".receivepack", ".vcs", ".proxy"]
                .iter()
                .any(|suffix| key.ends_with(suffix)))
}

fn validate_remote_url(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.starts_with('-')
        || value.chars().any(char::is_control)
        || value.contains("::")
        || value.starts_with(r"\\")
    {
        return Err(IntegrationError::new(
            "GIT_REPOSITORY_UNTRUSTED",
            "remote URL uses a forbidden helper or malformed transport",
        ));
    }

    // A drive-qualified local mirror is data, not a remote helper. UNC paths
    // remain forbidden because merely probing them may disclose OS credentials.
    if Path::new(value).is_absolute() && !value.starts_with("//") {
        return Ok(());
    }

    if let Ok(url) = url::Url::parse(value) {
        let valid = match url.scheme() {
            "https" => {
                url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            }
            "ssh" => {
                url.host_str().is_some()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url
                        .username()
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                    && !url.username().starts_with('-')
                    && url.path().len() > 1
            }
            "file" => {
                matches!(url.host_str(), None | Some("localhost"))
                    && url.password().is_none()
                    && url.username().is_empty()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && !url.path().is_empty()
            }
            _ => false,
        };
        if valid {
            return Ok(());
        }
        return Err(IntegrationError::new(
            "GIT_REPOSITORY_UNTRUSTED",
            "remote URL transport is not an allowlisted HTTPS, SSH, or local-file form",
        ));
    }

    // Git's SCP-like SSH form deliberately has no URI scheme.
    if let Some((authority, path)) = value.split_once(':') {
        let (user, host) = authority
            .split_once('@')
            .map_or((None, authority), |(user, host)| (Some(user), host));
        let safe_component = |part: &str| {
            !part.is_empty()
                && !part.starts_with('-')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        let safe_path = !path.is_empty()
            && !path.starts_with('-')
            && path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._~-".contains(&byte));
        if safe_component(host) && user.is_none_or(safe_component) && safe_path {
            return Ok(());
        }
        return Err(IntegrationError::new(
            "GIT_REPOSITORY_UNTRUSTED",
            "SCP-like remote URL is malformed",
        ));
    }

    // Plain paths use Git's built-in local transport. They are retained for
    // local mirrors and test fixtures and cannot name a remote helper.
    Ok(())
}

fn attributes_enable_filter(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        line.split_whitespace().skip(1).any(|attribute| {
            let attribute = attribute.trim_start_matches(['-', '!']);
            attribute == "filter" || attribute.starts_with("filter=")
        })
    })
}

fn changed_refs(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> BTreeMap<String, RefChange> {
    let mut result = BTreeMap::new();
    for key in before.keys().chain(after.keys()) {
        if result.contains_key(key) {
            continue;
        }
        let old = before.get(key).cloned();
        let new = after.get(key).cloned();
        if old != new {
            result.insert(key.clone(), RefChange { old, new });
        }
    }
    result
}

/// Fetch tag behavior.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagPolicy {
    /// Respect remote configuration.
    #[default]
    Auto,
    /// Fetch all tags.
    All,
    /// Fetch no tags.
    None,
}

/// Explicit non-destructive fetch options.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchOptions {
    /// Prune remote-tracking refs only when explicitly enabled.
    pub prune: bool,
    /// Tag behavior.
    pub tags: TagPolicy,
}

/// Old/new identity for one changed remote-tracking ref.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefChange {
    /// Previous full OID, absent for a new ref.
    pub old: Option<String>,
    /// New full OID, absent for a removed ref.
    pub new: Option<String>,
}

/// Audit receipt for a fetch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchReceipt {
    /// Configured remote name, never its URL.
    pub remote: String,
    /// Remote-tracking refs before fetch.
    pub started_from: BTreeMap<String, String>,
    /// Completion timestamp as Unix epoch milliseconds.
    pub finished_at_epoch_ms: u128,
    /// Ref changes caused by fetch.
    pub changed_refs: BTreeMap<String, RefChange>,
    /// Bounded process evidence.
    pub diagnostics: CommandDiagnostics,
}

/// Audit receipt for an ff-only update.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReceipt {
    /// HEAD before update.
    pub before: ExactCommit,
    /// HEAD after update.
    pub after: ExactCommit,
    /// Updated local branch.
    pub branch: String,
    /// Explicit upstream ref.
    pub upstream: String,
    /// Completion timestamp as Unix epoch milliseconds.
    pub completed_at_epoch_ms: u128,
    /// Bounded process evidence.
    pub diagnostics: CommandDiagnostics,
}

/// Lifecycle recorded for an isolated build worktree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeReceiptState {
    /// Worktree exists and is eligible for build input.
    Active,
}

/// Durable creation receipt for an isolated exact-commit worktree.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildWorktreeReceipt {
    /// Source repository top level.
    pub repository_root: PathBuf,
    /// Dedicated cache root.
    pub cache_root: PathBuf,
    /// Direct child containing the worktree.
    pub path: PathBuf,
    /// Exact detached commit.
    pub commit: ExactCommit,
    /// Creation timestamp as Unix epoch milliseconds.
    pub created_at_epoch_ms: u128,
    /// Receipt state.
    pub state: WorktreeReceiptState,
}

impl BuildWorktreeReceipt {
    fn validate_for(&self, client: &GitClient) -> Result<()> {
        let repository_root = fs::canonicalize(&self.repository_root).map_err(|error| {
            IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string())
        })?;
        let cache_root = fs::canonicalize(&self.cache_root).map_err(|error| {
            IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string())
        })?;
        let path = fs::canonicalize(&self.path).map_err(|error| {
            IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string())
        })?;
        if repository_root != client.root
            || path.parent() != Some(cache_root.as_path())
            || self.state != WorktreeReceiptState::Active
        {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_RECEIPT_INVALID",
                "receipt does not identify an owned direct-child worktree",
            ));
        }
        let persisted = read_worktree_receipt(self)?;
        if persisted.path != self.path
            || persisted.commit != self.commit
            || persisted.repository_root != self.repository_root
        {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_RECEIPT_INVALID",
                "persisted worktree receipt does not match cleanup request",
            ));
        }
        let worktree_client = GitClient {
            executable: client.executable.clone(),
            root: path,
            limits: client.limits,
        };
        let state = worktree_client.inspect()?;
        if state.head != self.commit
            || state.operation.is_some()
            || state.branch.is_some()
            || !state.is_clean()
        {
            return Err(IntegrationError::new(
                "GIT_WORKTREE_RECEIPT_INVALID",
                "worktree commit, detached state, cleanliness, or operation state changed",
            ));
        }
        Ok(())
    }
}

fn receipt_path(receipt: &BuildWorktreeReceipt) -> Result<PathBuf> {
    let name = receipt
        .path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", "worktree name is not UTF-8")
        })?;
    Ok(receipt
        .cache_root
        .join(format!("{name}.neuman-worktree.json")))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| IntegrationError::new("RECEIPT_SERIALIZE_FAILED", error.to_string()))?;
    let temporary = path.with_extension(format!("tmp-{}", epoch_millis()));
    fs::write(&temporary, bytes)
        .map_err(|error| IntegrationError::new("RECEIPT_WRITE_FAILED", error.to_string()))?;
    fs::rename(&temporary, path)
        .map_err(|error| IntegrationError::new("RECEIPT_WRITE_FAILED", error.to_string()))
}

fn write_worktree_receipt(receipt: &BuildWorktreeReceipt) -> Result<()> {
    write_json_atomic(&receipt_path(receipt)?, receipt)
}

fn read_worktree_receipt(receipt: &BuildWorktreeReceipt) -> Result<BuildWorktreeReceipt> {
    let bytes = fs::read(receipt_path(receipt)?).map_err(|error| {
        IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string())
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", error.to_string()))
}

fn write_cleanup_receipt(
    receipt: &BuildWorktreeReceipt,
    cleanup: &WorktreeCleanupReceipt,
) -> Result<()> {
    let name = receipt
        .path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            IntegrationError::new("GIT_WORKTREE_RECEIPT_INVALID", "worktree name is not UTF-8")
        })?;
    write_json_atomic(
        &receipt
            .cache_root
            .join(format!("{name}.neuman-cleanup.json")),
        cleanup,
    )
}

/// Cleanup evidence written only after artifact persistence was asserted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCleanupReceipt {
    /// Former worktree path.
    pub path: PathBuf,
    /// Exact commit assembled there.
    pub commit: ExactCommit,
    /// Caller assertion that artifact references were durable first.
    pub artifact_persisted: bool,
    /// Whether the directory is absent after cleanup.
    pub removed: bool,
    /// Completion timestamp as Unix epoch milliseconds.
    pub completed_at_epoch_ms: u128,
    /// Bounded process evidence.
    pub diagnostics: CommandDiagnostics,
}

/// Read-only orphan/reconciliation observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeReconciliation {
    /// Persisted creation receipt.
    pub receipt: BuildWorktreeReceipt,
    /// Whether its filesystem path exists.
    pub exists: bool,
    /// Whether Git still registers the worktree.
    pub registered: bool,
    /// Inconsistent state requiring a user-visible decision.
    pub requires_operator_review: bool,
}

/// Immutable Rojo binary pin from a trusted lockfile.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoPin {
    /// Executable path selected by the lockfile/tool manager.
    pub executable: PathBuf,
    /// Exact semantic version, for example `7.7.1`.
    pub version: String,
    /// Lowercase SHA-256 digest of the executable file.
    pub sha256: String,
}

/// A verified Rojo executable safe to place into a build/serve plan.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedRojo {
    /// Canonical executable path.
    executable: PathBuf,
    /// Exact verified version.
    version: String,
    /// Verified lowercase SHA-256.
    sha256: String,
}

impl VerifiedRojo {
    /// Verify file identity and `rojo --version` without upgrading or searching `PATH`.
    pub fn verify(pin: &RojoPin, limits: ProcessLimits) -> Result<Self> {
        validate_version(&pin.version)?;
        if pin.sha256.len() != 64
            || !pin
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(IntegrationError::new(
                "GIT_ROJO_VERSION_MISMATCH",
                "Rojo SHA-256 pin must be 64 lowercase hexadecimal characters",
            ));
        }
        let executable = fs::canonicalize(&pin.executable).map_err(|error| {
            IntegrationError::new("GIT_ROJO_VERSION_MISMATCH", error.to_string())
        })?;
        if !fs::metadata(&executable)
            .map_err(|error| IntegrationError::new("GIT_ROJO_VERSION_MISMATCH", error.to_string()))?
            .is_file()
        {
            return Err(IntegrationError::new(
                "GIT_ROJO_VERSION_MISMATCH",
                "Rojo executable is not a regular file",
            ));
        }
        let actual_hash = sha256_file(&executable)?;
        if actual_hash != pin.sha256 {
            return Err(IntegrationError::new(
                "GIT_ROJO_VERSION_MISMATCH",
                "Rojo executable checksum does not match lockfile",
            ));
        }
        let mut command = Command::new(&executable);
        allowed_environment(&mut command);
        command.arg("--version");
        let output = ensure_success(
            run_bounded(command, "rojo version verification", limits)?,
            "GIT_ROJO_VERSION_MISMATCH",
            "Rojo version command failed",
        )?;
        let actual_version = parse_rojo_version(output_text(&output))?;
        if actual_version != pin.version {
            return Err(IntegrationError::command(
                "GIT_ROJO_VERSION_MISMATCH",
                format!("expected Rojo {}, found {actual_version}", pin.version),
                output.diagnostics,
            ));
        }
        Ok(Self {
            executable,
            version: pin.version.clone(),
            sha256: actual_hash,
        })
    }

    /// Canonical verified executable path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Exact verified version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Verified lowercase SHA-256 digest.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

fn validate_version(version: &str) -> Result<()> {
    if version.is_empty()
        || version.len() > 64
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(IntegrationError::new(
            "GIT_ROJO_VERSION_MISMATCH",
            "invalid exact Rojo version",
        ));
    }
    Ok(())
}

fn parse_rojo_version(output: &str) -> Result<String> {
    let mut words = output.split_whitespace();
    let product = words.next().unwrap_or_default();
    let version = words.next().unwrap_or_default();
    if !product.eq_ignore_ascii_case("rojo") || words.next().is_some() {
        return Err(IntegrationError::new(
            "GIT_ROJO_VERSION_MISMATCH",
            "unexpected `rojo --version` response",
        ));
    }
    validate_version(version)?;
    Ok(version.to_owned())
}

/// Rojo build output encoding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RojoBuildFormat {
    /// Binary Roblox place.
    Binary,
    /// XML Roblox place.
    Xml,
}

/// Fully validated immutable Rojo build plan.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoBuildPlan {
    /// Verified executable.
    pub rojo: VerifiedRojo,
    /// Exact detached worktree root.
    pub workspace_root: PathBuf,
    /// Canonical project file within the worktree.
    pub project_file: PathBuf,
    /// SHA-256 of project file bytes.
    pub project_sha256: String,
    /// New output file within the dedicated output root.
    pub output_file: PathBuf,
    /// Output encoding.
    pub format: RojoBuildFormat,
    /// Child process bounds.
    pub limits: ProcessLimits,
}

impl RojoBuildPlan {
    /// Validate all paths before a build process is started.
    pub fn create(
        rojo: VerifiedRojo,
        workspace_root: &Path,
        project_relative: &Path,
        output_root: &Path,
        output_relative: &Path,
        format: RojoBuildFormat,
        limits: ProcessLimits,
    ) -> Result<Self> {
        validate_relative_path(project_relative, "Rojo project path")?;
        validate_relative_path(output_relative, "Rojo output path")?;
        let workspace_root = fs::canonicalize(workspace_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        let project_file = canonical_inside(
            &workspace_root,
            &workspace_root.join(project_relative),
            "Rojo project",
        )?;
        if !fs::metadata(&project_file)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?
            .is_file()
        {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "Rojo project is not a regular file",
            ));
        }
        fs::create_dir_all(output_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        let output_root = fs::canonicalize(output_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        if output_root.starts_with(&workspace_root) || workspace_root.starts_with(&output_root) {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "build output root and exact input worktree must not contain one another",
            ));
        }
        let output_file = output_root.join(output_relative);
        if output_file.exists() {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "Rojo build output must not already exist",
            ));
        }
        let parent = output_file
            .parent()
            .ok_or_else(|| IntegrationError::new("PATH_UNSAFE", "output has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        let parent = fs::canonicalize(parent)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        if !parent.starts_with(&output_root) {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "Rojo output escapes output root",
            ));
        }
        let extension = output_file
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        let expected_extension = match format {
            RojoBuildFormat::Binary => "rbxl",
            RojoBuildFormat::Xml => "rbxlx",
        };
        if extension != expected_extension {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                format!("Rojo output must use .{expected_extension}"),
            ));
        }
        Ok(Self {
            rojo,
            workspace_root,
            project_file: project_file.clone(),
            project_sha256: sha256_file(&project_file)?,
            output_file,
            format,
            limits,
        })
    }

    /// Run `rojo build` and return immutable output evidence.
    pub fn execute(&self) -> Result<RojoBuildReceipt> {
        if sha256_file(&self.project_file)? != self.project_sha256 {
            return Err(IntegrationError::new(
                "GIT_SOURCEMAP_STALE",
                "Rojo project changed after plan validation",
            ));
        }
        if self.output_file.exists() {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "Rojo build output appeared after plan validation",
            ));
        }
        let mut command = Command::new(&self.rojo.executable);
        allowed_environment(&mut command);
        command
            .current_dir(&self.workspace_root)
            .arg("build")
            .arg(command_path(&self.project_file))
            .arg("--output")
            .arg(command_path(&self.output_file));
        let output = run_bounded(command, "rojo immutable build", self.limits)?;
        let diagnostics = output.diagnostics.clone();
        ensure_success(output, "GIT_ROJO_SERVER_FAILED", "Rojo build failed")?;
        let metadata = fs::symlink_metadata(&self.output_file).map_err(|error| {
            IntegrationError::new(
                "GIT_ROJO_SERVER_FAILED",
                format!("Rojo produced no output: {error}"),
            )
        })?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
        {
            return Err(IntegrationError::new(
                "GIT_ROJO_SERVER_FAILED",
                "Rojo output is empty, a symlink, or not a regular file",
            ));
        }
        Ok(RojoBuildReceipt {
            output_file: self.output_file.clone(),
            output_sha256: sha256_file(&self.output_file)?,
            output_size: metadata.len(),
            project_sha256: self.project_sha256.clone(),
            rojo_version: self.rojo.version.clone(),
            rojo_sha256: self.rojo.sha256.clone(),
            completed_at_epoch_ms: epoch_millis(),
            diagnostics,
        })
    }
}

/// Immutable Rojo assembly evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoBuildReceipt {
    /// Output path retained for downstream CAS ingestion.
    pub output_file: PathBuf,
    /// SHA-256 of output bytes.
    pub output_sha256: String,
    /// Output byte length.
    pub output_size: u64,
    /// Project file identity.
    pub project_sha256: String,
    /// Exact Rojo version.
    pub rojo_version: String,
    /// Exact Rojo executable identity.
    pub rojo_sha256: String,
    /// Completion timestamp as Unix epoch milliseconds.
    pub completed_at_epoch_ms: u128,
    /// Bounded process evidence.
    pub diagnostics: CommandDiagnostics,
}

/// Validated loopback-only Rojo serve plan.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoServePlan {
    /// Verified executable.
    pub rojo: VerifiedRojo,
    /// Workspace root.
    pub workspace_root: PathBuf,
    /// Canonical project file.
    pub project_file: PathBuf,
    /// SHA-256 of project file bytes.
    pub project_sha256: String,
    /// Loopback port.
    pub port: u16,
    /// Maximum explicit restarts during this supervisor lifetime.
    pub max_restarts: u32,
    /// Retained bytes across each log stream.
    pub log_limit_bytes: usize,
}

impl RojoServePlan {
    /// Create a loopback serve plan for a project inside the workspace.
    pub fn create(
        rojo: VerifiedRojo,
        workspace_root: &Path,
        project_relative: &Path,
        port: u16,
        max_restarts: u32,
        log_limit_bytes: usize,
    ) -> Result<Self> {
        if port == 0 {
            return Err(IntegrationError::new(
                "GIT_ROJO_SERVER_FAILED",
                "Rojo port must be nonzero",
            ));
        }
        validate_relative_path(project_relative, "Rojo project path")?;
        let workspace_root = fs::canonicalize(workspace_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        let project_file = canonical_inside(
            &workspace_root,
            &workspace_root.join(project_relative),
            "Rojo project",
        )?;
        if !fs::metadata(&project_file)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?
            .is_file()
        {
            return Err(IntegrationError::new(
                "PATH_UNSAFE",
                "Rojo project is not a regular file",
            ));
        }
        Ok(Self {
            rojo,
            workspace_root,
            project_file: project_file.clone(),
            project_sha256: sha256_file(&project_file)?,
            port,
            max_restarts,
            log_limit_bytes: log_limit_bytes.clamp(4096, 4 * 1024 * 1024),
        })
    }
}

#[derive(Default)]
struct LiveLog {
    bytes: Vec<u8>,
    truncated: bool,
}

fn drain_live<R: Read + Send + 'static>(mut reader: R, capture: Arc<Mutex<LiveLog>>, limit: usize) {
    thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        while let Ok(count) = reader.read(&mut chunk) {
            if count == 0 {
                break;
            }
            if let Ok(mut log) = capture.lock() {
                let keep = limit.saturating_sub(log.bytes.len()).min(count);
                log.bytes.extend_from_slice(&chunk[..keep]);
                log.truncated |= keep != count;
            }
        }
    });
}

/// Observable Rojo server state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RojoServerState {
    /// Child is running but its loopback port is not reachable yet.
    Starting,
    /// Child is running and its loopback port accepts a TCP connection.
    Healthy,
    /// Child exited.
    Exited,
    /// Supervisor stopped the child.
    Stopped,
}

/// Snapshot from a supervised server.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoServerSnapshot {
    /// Current lifecycle state.
    pub state: RojoServerState,
    /// OS process ID when running.
    pub pid: Option<u32>,
    /// Loopback port.
    pub port: u16,
    /// Number of explicit restarts.
    pub restart_attempts: u32,
    /// Exit code after exit.
    pub exit_code: Option<i32>,
    /// Bounded stdout collected so far.
    pub stdout: String,
    /// Bounded stderr collected so far.
    pub stderr: String,
    /// Whether either stream was truncated.
    pub logs_truncated: bool,
}

/// Single-owner Rojo child supervisor with bounded logs and restarts.
pub struct RojoSupervisor {
    plan: RojoServePlan,
    child: Option<Child>,
    stdout: Arc<Mutex<LiveLog>>,
    stderr: Arc<Mutex<LiveLog>>,
    restarts: u32,
    last_exit: Option<i32>,
    stopped: bool,
}

impl fmt::Debug for RojoSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RojoSupervisor")
            .field("port", &self.plan.port)
            .field("restarts", &self.restarts)
            .field("pid", &self.child.as_ref().map(Child::id))
            .finish_non_exhaustive()
    }
}

impl RojoSupervisor {
    /// Start exactly one Rojo child from a validated plan.
    pub fn start(plan: RojoServePlan) -> Result<Self> {
        let mut supervisor = Self {
            plan,
            child: None,
            stdout: Arc::new(Mutex::new(LiveLog::default())),
            stderr: Arc::new(Mutex::new(LiveLog::default())),
            restarts: 0,
            last_exit: None,
            stopped: false,
        };
        supervisor.spawn_child()?;
        Ok(supervisor)
    }

    fn spawn_child(&mut self) -> Result<()> {
        if sha256_file(&self.plan.project_file)? != self.plan.project_sha256 {
            return Err(IntegrationError::new(
                "GIT_SOURCEMAP_STALE",
                "Rojo project changed after serve plan validation",
            ));
        }
        if self.child.is_some() {
            return Err(IntegrationError::new(
                "GIT_ROJO_SERVER_FAILED",
                "a Rojo child is already owned by this supervisor",
            ));
        }
        let mut command = Command::new(&self.plan.rojo.executable);
        allowed_environment(&mut command);
        command
            .current_dir(&self.plan.workspace_root)
            .arg("serve")
            .arg(command_path(&self.plan.project_file))
            .arg("--address")
            .arg(Ipv4Addr::LOCALHOST.to_string())
            .arg("--port")
            .arg(self.plan.port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| IntegrationError::new("GIT_ROJO_SERVER_FAILED", error.to_string()))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            IntegrationError::new("GIT_ROJO_SERVER_FAILED", "Rojo stdout unavailable")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            IntegrationError::new("GIT_ROJO_SERVER_FAILED", "Rojo stderr unavailable")
        })?;
        drain_live(stdout, Arc::clone(&self.stdout), self.plan.log_limit_bytes);
        drain_live(stderr, Arc::clone(&self.stderr), self.plan.log_limit_bytes);
        self.child = Some(child);
        self.stopped = false;
        Ok(())
    }

    /// Poll process and loopback health without blocking.
    pub fn snapshot(&mut self) -> Result<RojoServerSnapshot> {
        let mut state = if self.stopped {
            RojoServerState::Stopped
        } else {
            RojoServerState::Exited
        };
        let mut pid = None;
        let mut exit_code = self.last_exit;
        if let Some(child) = self.child.as_mut() {
            pid = Some(child.id());
            if let Some(status) = child.try_wait().map_err(|error| {
                IntegrationError::new("GIT_ROJO_SERVER_FAILED", error.to_string())
            })? {
                self.last_exit = status.code();
                exit_code = status.code();
                self.child = None;
                state = RojoServerState::Exited;
            } else {
                let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.plan.port);
                state = if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok()
                {
                    RojoServerState::Healthy
                } else {
                    RojoServerState::Starting
                };
            }
        }
        let (stdout, stdout_truncated) = log_snapshot(&self.stdout);
        let (stderr, stderr_truncated) = log_snapshot(&self.stderr);
        Ok(RojoServerSnapshot {
            state,
            pid,
            port: self.plan.port,
            restart_attempts: self.restarts,
            exit_code,
            stdout,
            stderr,
            logs_truncated: stdout_truncated || stderr_truncated,
        })
    }

    /// Explicitly restart after an exit, bounded by the plan.
    pub fn restart(&mut self) -> Result<()> {
        if self.child.is_some() {
            return Err(IntegrationError::new(
                "GIT_ROJO_SERVER_FAILED",
                "running server must be stopped before restart",
            ));
        }
        if self.restarts >= self.plan.max_restarts {
            return Err(IntegrationError::new(
                "GIT_ROJO_SERVER_FAILED",
                "Rojo restart budget exhausted",
            ));
        }
        self.restarts += 1;
        self.stdout = Arc::new(Mutex::new(LiveLog::default()));
        self.stderr = Arc::new(Mutex::new(LiveLog::default()));
        self.spawn_child()
    }

    /// Terminate the owned child and wait for it. No unrelated PID is touched.
    pub fn stop(&mut self) -> Result<RojoServerSnapshot> {
        if let Some(mut child) = self.child.take() {
            let status = if let Some(status) = child.try_wait().map_err(|error| {
                IntegrationError::new("GIT_ROJO_SERVER_FAILED", error.to_string())
            })? {
                status
            } else {
                child.kill().map_err(|error| {
                    IntegrationError::new("GIT_ROJO_SERVER_FAILED", error.to_string())
                })?;
                child.wait().map_err(|error| {
                    IntegrationError::new("GIT_ROJO_SERVER_FAILED", error.to_string())
                })?
            };
            self.last_exit = status.code();
        }
        self.stopped = true;
        self.snapshot()
    }
}

impl Drop for RojoSupervisor {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Stable identity for the single live Rojo session assigned to a workspace/place pair.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoSessionKey {
    /// Canonical workspace root.
    workspace_root: PathBuf,
    /// Manifest place key, not a Roblox numeric identifier.
    place_key: String,
    #[serde(skip)]
    workspace_identity: String,
}

impl PartialEq for RojoSessionKey {
    fn eq(&self, other: &Self) -> bool {
        self.workspace_identity == other.workspace_identity && self.place_key == other.place_key
    }
}

impl Eq for RojoSessionKey {}

impl PartialOrd for RojoSessionKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RojoSessionKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.workspace_identity, &self.place_key)
            .cmp(&(&other.workspace_identity, &other.place_key))
    }
}

impl RojoSessionKey {
    /// Canonicalize and validate a workspace/place identity.
    pub fn create(workspace_root: &Path, place_key: &str) -> Result<Self> {
        validate_simple_name(place_key, "place key")?;
        if place_key.contains('/') {
            return Err(IntegrationError::new(
                "GIT_ROJO_SESSION_INVALID",
                "place key must be one logical component",
            ));
        }
        let workspace_root = fs::canonicalize(workspace_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?;
        if !fs::metadata(&workspace_root)
            .map_err(|error| IntegrationError::new("PATH_UNAVAILABLE", error.to_string()))?
            .is_dir()
        {
            return Err(IntegrationError::new(
                "GIT_ROJO_SESSION_INVALID",
                "workspace root is not a directory",
            ));
        }
        let workspace_identity = normalized_workspace_identity(&workspace_root);
        Ok(Self {
            workspace_root,
            place_key: place_key.to_owned(),
            workspace_identity,
        })
    }

    /// Canonical workspace root.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Manifest place key.
    #[must_use]
    pub fn place_key(&self) -> &str {
        &self.place_key
    }
}

/// Inclusive loopback port range reserved by policy for managed Rojo sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoPortRange {
    /// First candidate port.
    start: u16,
    /// Last candidate port.
    end: u16,
}

impl<'de> Deserialize<'de> for RojoPortRange {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireRange {
            start: u16,
            end: u16,
        }

        let value = WireRange::deserialize(deserializer)?;
        Self::create(value.start, value.end).map_err(serde::de::Error::custom)
    }
}

impl RojoPortRange {
    /// Validate a non-privileged, bounded port range.
    pub fn create(start: u16, end: u16) -> Result<Self> {
        if start < 1024 || end < start || u32::from(end) - u32::from(start) > 16_383 {
            return Err(IntegrationError::new(
                "GIT_ROJO_SESSION_INVALID",
                "Rojo port range must be non-privileged, ordered, and contain at most 16384 ports",
            ));
        }
        Ok(Self { start, end })
    }

    /// First candidate port.
    #[must_use]
    pub fn start(self) -> u16 {
        self.start
    }

    /// Last candidate port.
    #[must_use]
    pub fn end(self) -> u16 {
        self.end
    }

    fn len(self) -> u32 {
        u32::from(self.end) - u32::from(self.start) + 1
    }
}

impl Default for RojoPortRange {
    fn default() -> Self {
        Self {
            start: 34_872,
            end: 34_971,
        }
    }
}

/// Desktop/daemon input for creating one managed live Rojo session.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoSessionStartRequest {
    /// Lockfile-derived executable identity. It is re-verified on every start request.
    pub pin: RojoPin,
    /// Workspace containing the project file.
    pub workspace_root: PathBuf,
    /// Normalized project path relative to `workspace_root`.
    pub project_relative: PathBuf,
    /// Manifest place key associated with the Studio session.
    pub place_key: String,
    /// Maximum controlled or crash restarts for this retained session.
    pub max_restarts: u32,
    /// Maximum retained bytes per child log stream.
    pub log_limit_bytes: usize,
}

/// Reason the most recently owned child exited.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RojoChildExitKind {
    /// Child exited without a manager stop/restart request.
    Unexpected,
    /// Manager intentionally stopped its owned child.
    RequestedStop,
    /// Manager intentionally stopped its owned child before a restart.
    RequestedRestart,
}

/// Durable status evidence for the most recently exited owned child.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoChildExitReport {
    /// Exit classification.
    pub kind: RojoChildExitKind,
    /// Platform exit code when one exists.
    pub exit_code: Option<i32>,
    /// Observation timestamp as Unix epoch milliseconds.
    pub observed_at_epoch_ms: u128,
}

/// Complete desktop-safe snapshot for one managed live session.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoSessionStatus {
    /// Stable workspace/place identity.
    pub key: RojoSessionKey,
    /// Stable local session identifier derived from the key.
    pub session_id: String,
    /// Hash of workspace, place, project file, and pinned Rojo identity.
    pub context_id: String,
    /// Current child/server state.
    pub state: RojoServerState,
    /// Owned process ID while the child is live. It is evidence, never a kill target.
    pub pid: Option<u32>,
    /// Deterministically selected loopback port.
    pub port: u16,
    /// Number of controlled restart attempts.
    pub restart_attempts: u32,
    /// Exact project-file SHA-256.
    pub project_sha256: String,
    /// Exact verified Rojo version.
    pub rojo_version: String,
    /// Exact verified Rojo executable SHA-256.
    pub rojo_sha256: String,
    /// Timestamp when the currently attempted child was started.
    pub started_at_epoch_ms: u128,
    /// Timestamp for this observation.
    pub observed_at_epoch_ms: u128,
    /// Most recent child exit, retained across restart.
    pub last_exit: Option<RojoChildExitReport>,
    /// Bounded redacted stdout.
    pub stdout: String,
    /// Bounded redacted stderr.
    pub stderr: String,
    /// Whether either child stream exceeded its retention bound.
    pub logs_truncated: bool,
}

/// Result of an idempotent session start request.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RojoSessionStartOutcome {
    /// True only when this call created a new child/session record.
    pub created: bool,
    /// Current session status.
    pub status: RojoSessionStatus,
}

struct ManagedRojoSession {
    key: RojoSessionKey,
    session_id: String,
    context_id: String,
    project_sha256: String,
    rojo_version: String,
    rojo_sha256: String,
    started_at_epoch_ms: u128,
    last_state: RojoServerState,
    last_exit: Option<RojoChildExitReport>,
    supervisor: RojoSupervisor,
}

/// Registry and lifecycle owner for at most one Rojo child per workspace/place key.
///
/// Store this object behind the desktop command boundary (for example, in a Tauri
/// `Mutex`). Its maps and child handles must not be exposed to the webview.
pub struct RojoSessionManager {
    sessions: BTreeMap<RojoSessionKey, ManagedRojoSession>,
    port_range: RojoPortRange,
    verification_limits: ProcessLimits,
}

impl fmt::Debug for RojoSessionManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RojoSessionManager")
            .field("session_count", &self.sessions.len())
            .field("port_range", &self.port_range)
            .finish_non_exhaustive()
    }
}

impl Default for RojoSessionManager {
    fn default() -> Self {
        Self::new(RojoPortRange::default(), ProcessLimits::default())
    }
}

impl RojoSessionManager {
    /// Create an empty manager with policy-selected ports and verification bounds.
    #[must_use]
    pub fn new(port_range: RojoPortRange, verification_limits: ProcessLimits) -> Self {
        Self {
            sessions: BTreeMap::new(),
            port_range,
            verification_limits,
        }
    }

    /// Start a new session or return the existing identical workspace/place session.
    pub fn start(&mut self, request: &RojoSessionStartRequest) -> Result<RojoSessionStartOutcome> {
        let key = RojoSessionKey::create(&request.workspace_root, &request.place_key)?;
        let rojo = VerifiedRojo::verify(&request.pin, self.verification_limits)?;
        let provisional_port = self
            .sessions
            .get(&key)
            .map_or(self.port_range.start, |session| {
                session.supervisor.plan.port
            });
        let mut plan = RojoServePlan::create(
            rojo,
            &key.workspace_root,
            &request.project_relative,
            provisional_port,
            request.max_restarts,
            request.log_limit_bytes,
        )?;
        let context_id = session_context_id(&key, &plan);

        if let Some(session) = self.sessions.get_mut(&key) {
            let status = refresh_managed_session(session)?;
            if session.context_id != context_id {
                return Err(IntegrationError::new(
                    "GIT_ROJO_SESSION_CONFLICT",
                    "a different project or pinned Rojo context already owns this workspace/place; stop and remove it before replacement",
                ));
            }
            return Ok(RojoSessionStartOutcome {
                created: false,
                status,
            });
        }

        plan.port = self.select_port(&key)?;
        let project_sha256 = plan.project_sha256.clone();
        let rojo_version = plan.rojo.version().to_owned();
        let rojo_sha256 = plan.rojo.sha256().to_owned();
        let port = plan.port;
        let supervisor = RojoSupervisor::start(plan)?;
        let started_at_epoch_ms = epoch_millis();
        let session = ManagedRojoSession {
            key: key.clone(),
            session_id: session_id(&key),
            context_id,
            project_sha256,
            rojo_version,
            rojo_sha256,
            started_at_epoch_ms,
            last_state: RojoServerState::Starting,
            last_exit: None,
            supervisor,
        };
        self.sessions.insert(key.clone(), session);
        let status = self.status_by_key(&key)?;
        debug_assert_eq!(status.port, port);
        Ok(RojoSessionStartOutcome {
            created: true,
            status,
        })
    }

    /// Observe one retained session by its canonical key.
    pub fn status_by_key(&mut self, key: &RojoSessionKey) -> Result<RojoSessionStatus> {
        let session = self.sessions.get_mut(key).ok_or_else(|| {
            IntegrationError::new("GIT_ROJO_SESSION_NOT_FOUND", "Rojo session was not found")
        })?;
        refresh_managed_session(session)
    }

    /// Canonicalize a workspace/place pair and observe its retained session.
    pub fn status(&mut self, workspace_root: &Path, place_key: &str) -> Result<RojoSessionStatus> {
        let key = RojoSessionKey::create(workspace_root, place_key)?;
        self.status_by_key(&key)
    }

    /// Observe every retained session in deterministic key order.
    pub fn list(&mut self) -> Result<Vec<RojoSessionStatus>> {
        self.sessions
            .values_mut()
            .map(refresh_managed_session)
            .collect()
    }

    /// Stop only the child handle owned by the requested session.
    pub fn stop(&mut self, key: &RojoSessionKey) -> Result<RojoSessionStatus> {
        let session = self.sessions.get_mut(key).ok_or_else(|| {
            IntegrationError::new("GIT_ROJO_SESSION_NOT_FOUND", "Rojo session was not found")
        })?;
        let before = refresh_managed_session(session)?;
        let had_live_child = session.supervisor.child.is_some();
        let snapshot = session.supervisor.stop()?;
        if had_live_child {
            session.last_exit = Some(RojoChildExitReport {
                kind: RojoChildExitKind::RequestedStop,
                exit_code: snapshot.exit_code,
                observed_at_epoch_ms: epoch_millis(),
            });
        } else if before.state != RojoServerState::Exited {
            session.last_exit = before.last_exit;
        }
        session.last_state = RojoServerState::Stopped;
        managed_status(session, snapshot)
    }

    /// Perform a controlled stop/start of the owned child within its restart budget.
    pub fn restart(&mut self, key: &RojoSessionKey) -> Result<RojoSessionStatus> {
        let session = self.sessions.get_mut(key).ok_or_else(|| {
            IntegrationError::new("GIT_ROJO_SESSION_NOT_FOUND", "Rojo session was not found")
        })?;
        let before = refresh_managed_session(session)?;
        if session.supervisor.child.is_some() {
            let stopped = session.supervisor.stop()?;
            session.last_exit = Some(RojoChildExitReport {
                kind: RojoChildExitKind::RequestedRestart,
                exit_code: stopped.exit_code,
                observed_at_epoch_ms: epoch_millis(),
            });
        } else if before.state == RojoServerState::Exited && session.last_exit.is_none() {
            session.last_exit = Some(RojoChildExitReport {
                kind: RojoChildExitKind::Unexpected,
                exit_code: before
                    .last_exit
                    .as_ref()
                    .and_then(|report| report.exit_code),
                observed_at_epoch_ms: epoch_millis(),
            });
        }
        session.supervisor.restart()?;
        session.started_at_epoch_ms = epoch_millis();
        session.last_state = RojoServerState::Starting;
        refresh_managed_session(session)
    }

    /// Stop every owned child, continuing cleanup even if one stop fails.
    pub fn stop_all(&mut self) -> Result<Vec<RojoSessionStatus>> {
        let keys: Vec<_> = self.sessions.keys().cloned().collect();
        let mut statuses = Vec::with_capacity(keys.len());
        let mut first_error = None;
        for key in keys {
            match self.stop(&key) {
                Ok(status) => statuses.push(status),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(statuses)
        }
    }

    /// Remove an exited/stopped record after proving that no child handle remains.
    pub fn remove_inactive(&mut self, key: &RojoSessionKey) -> Result<bool> {
        let Some(session) = self.sessions.get_mut(key) else {
            return Ok(false);
        };
        let status = refresh_managed_session(session)?;
        if session.supervisor.child.is_some()
            || !matches!(
                status.state,
                RojoServerState::Exited | RojoServerState::Stopped
            )
        {
            return Err(IntegrationError::new(
                "GIT_ROJO_SESSION_ACTIVE",
                "stop the owned Rojo child before removing its session record",
            ));
        }
        self.sessions.remove(key);
        Ok(true)
    }

    fn select_port(&self, key: &RojoSessionKey) -> Result<u16> {
        let mut hasher = Sha256::new();
        hash_session_field(&mut hasher, b"neuman-rojo-port-v1");
        hash_session_field(
            &mut hasher,
            normalized_workspace_identity(&key.workspace_root).as_bytes(),
        );
        hash_session_field(&mut hasher, key.place_key.as_bytes());
        let digest = hasher.finalize();
        let seed = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        let range_len = self.port_range.len();
        let reserved: Vec<u16> = self
            .sessions
            .values()
            .map(|session| session.supervisor.plan.port)
            .collect();
        for step in 0..range_len {
            let offset = seed.wrapping_add(step) % range_len;
            let port = u32::from(self.port_range.start) + offset;
            let port = u16::try_from(port).expect("validated u16 port range");
            if reserved.contains(&port) {
                continue;
            }
            let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            if TcpListener::bind(address).is_ok() {
                return Ok(port);
            }
        }
        Err(IntegrationError::new(
            "GIT_ROJO_PORT_UNAVAILABLE",
            "no loopback port in the configured Rojo range is currently available",
        ))
    }
}

impl Drop for RojoSessionManager {
    fn drop(&mut self) {
        for session in self.sessions.values_mut() {
            let _ = session.supervisor.stop();
        }
    }
}

fn refresh_managed_session(session: &mut ManagedRojoSession) -> Result<RojoSessionStatus> {
    let snapshot = session.supervisor.snapshot()?;
    if snapshot.state == RojoServerState::Exited
        && !matches!(
            session.last_state,
            RojoServerState::Exited | RojoServerState::Stopped
        )
    {
        session.last_exit = Some(RojoChildExitReport {
            kind: RojoChildExitKind::Unexpected,
            exit_code: snapshot.exit_code,
            observed_at_epoch_ms: epoch_millis(),
        });
    }
    session.last_state = snapshot.state;
    managed_status(session, snapshot)
}

fn managed_status(
    session: &ManagedRojoSession,
    snapshot: RojoServerSnapshot,
) -> Result<RojoSessionStatus> {
    if snapshot.port != session.supervisor.plan.port {
        return Err(IntegrationError::new(
            "GIT_ROJO_SESSION_INVALID",
            "supervisor port no longer matches retained session context",
        ));
    }
    Ok(RojoSessionStatus {
        key: session.key.clone(),
        session_id: session.session_id.clone(),
        context_id: session.context_id.clone(),
        state: snapshot.state,
        pid: snapshot.pid,
        port: snapshot.port,
        restart_attempts: snapshot.restart_attempts,
        project_sha256: session.project_sha256.clone(),
        rojo_version: session.rojo_version.clone(),
        rojo_sha256: session.rojo_sha256.clone(),
        started_at_epoch_ms: session.started_at_epoch_ms,
        observed_at_epoch_ms: epoch_millis(),
        last_exit: session.last_exit.clone(),
        stdout: snapshot.stdout,
        stderr: snapshot.stderr,
        logs_truncated: snapshot.logs_truncated,
    })
}

fn session_id(key: &RojoSessionKey) -> String {
    let mut hasher = Sha256::new();
    hash_session_field(&mut hasher, b"neuman-rojo-session-v1");
    hash_session_field(
        &mut hasher,
        normalized_workspace_identity(&key.workspace_root).as_bytes(),
    );
    hash_session_field(&mut hasher, key.place_key.as_bytes());
    format!("rojo-session:{}", &hex::encode(hasher.finalize())[..32])
}

fn session_context_id(key: &RojoSessionKey, plan: &RojoServePlan) -> String {
    let mut hasher = Sha256::new();
    hash_session_field(&mut hasher, b"neuman-rojo-context-v1");
    hash_session_field(
        &mut hasher,
        normalized_workspace_identity(&key.workspace_root).as_bytes(),
    );
    hash_session_field(&mut hasher, key.place_key.as_bytes());
    hash_session_field(
        &mut hasher,
        normalized_workspace_identity(&plan.project_file).as_bytes(),
    );
    hash_session_field(&mut hasher, plan.project_sha256.as_bytes());
    hash_session_field(&mut hasher, plan.rojo.version().as_bytes());
    hash_session_field(&mut hasher, plan.rojo.sha256().as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn normalized_workspace_identity(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value.into_owned()
    }
}

fn hash_session_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn log_snapshot(capture: &Arc<Mutex<LiveLog>>) -> (String, bool) {
    let Ok(log) = capture.lock() else {
        return ("[log lock poisoned]".to_owned(), true);
    };
    (
        redact_diagnostics(&String::from_utf8_lossy(&log.bytes)),
        log.truncated,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "NeuMan Test"]);
        fs::write(
            temp.path().join("default.project.json"),
            b"{\"name\":\"test\",\"tree\":{\"$className\":\"DataModel\"}}",
        )
        .unwrap();
        git(temp.path(), &["add", "default.project.json"]);
        git(temp.path(), &["commit", "-qm", "initial"]);
        temp
    }

    fn fake_rojo(directory: &Path) -> RojoPin {
        let source = directory.join("fake_rojo.rs");
        let executable = directory.join(format!("fake-rojo{}", std::env::consts::EXE_SUFFIX));
        fs::write(
            &source,
            r#"
use std::env;
use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::process;
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        println!("Rojo 7.7.1");
        return;
    }
    if args.get(1).map(String::as_str) != Some("serve") {
        eprintln!("unsupported fake command");
        process::exit(2);
    }
    let project = args.get(2).expect("project path");
    let contents = fs::read_to_string(project).expect("project readable");
    if contents.contains("exitImmediately") {
        eprintln!("intentional fake crash");
        process::exit(17);
    }
    let port_index = args.iter().position(|arg| arg == "--port").expect("port argument");
    let port: u16 = args[port_index + 1].parse().expect("numeric port");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("loopback bind");
    listener.set_nonblocking(true).expect("nonblocking");
    println!("fake Rojo listening on {port}");
    loop {
        let _ = listener.accept();
        thread::sleep(Duration::from_millis(10));
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
            .expect("rustc starts");
        assert!(
            output.status.success(),
            "fake Rojo compilation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        RojoPin {
            sha256: sha256_file(&executable).unwrap(),
            executable,
            version: "7.7.1".to_owned(),
        }
    }

    fn session_request(
        pin: &RojoPin,
        workspace: &Path,
        project: &str,
        place: &str,
    ) -> RojoSessionStartRequest {
        RojoSessionStartRequest {
            pin: pin.clone(),
            workspace_root: workspace.to_path_buf(),
            project_relative: PathBuf::from(project),
            place_key: place.to_owned(),
            max_restarts: 3,
            log_limit_bytes: 16 * 1024,
        }
    }

    fn wait_for_session_state(
        manager: &mut RojoSessionManager,
        key: &RojoSessionKey,
        expected: RojoServerState,
    ) -> RojoSessionStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = manager.status_by_key(key).unwrap();
            if status.state == expected {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "session did not reach {expected:?}: {status:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn redacts_url_userinfo_and_authorization() {
        let redacted = redact_diagnostics(
            "fatal https://alice:secret@example.com/repo\nAuthorization: Bearer abc",
        );
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("Bearer"));
        assert!(redacted.contains("[redacted]@example.com"));
    }

    #[test]
    fn validates_exact_object_lengths() {
        assert!(ExactCommit::parse(&"a".repeat(40), GitObjectFormat::Sha1).is_ok());
        assert!(ExactCommit::parse(&"a".repeat(64), GitObjectFormat::Sha256).is_ok());
        assert!(ExactCommit::parse(&"A".repeat(40), GitObjectFormat::Sha1).is_err());
        assert!(ExactCommit::parse("main", GitObjectFormat::Sha1).is_err());
        assert!(serde_json::from_str::<ExactCommit>("\"main\"").is_err());
    }

    #[test]
    fn rejects_unsafe_relative_paths() {
        assert!(validate_relative_path(Path::new("default.project.json"), "test").is_ok());
        assert!(validate_relative_path(Path::new("../secret"), "test").is_err());
        assert!(validate_relative_path(Path::new("/absolute"), "test").is_err());
    }

    #[test]
    fn parses_only_exact_rojo_version_shape() {
        assert_eq!(parse_rojo_version("Rojo 7.7.1\n").unwrap(), "7.7.1");
        assert!(parse_rojo_version("rojo version 7.7.1").is_err());
        assert!(parse_rojo_version("malicious 7.7.1").is_err());
    }

    #[test]
    fn integration_inspects_and_dirty_state_fails_closed() {
        if !git_available() {
            return;
        }
        let repo = repository();
        let client = GitClient::open(repo.path()).unwrap();
        let clean = client.inspect().unwrap();
        assert!(clean.is_clean());
        assert!(clean.branch.is_some());
        fs::write(repo.path().join("untracked.txt"), b"dirty").unwrap();
        let dirty = client.inspect().unwrap();
        assert_eq!(dirty.untracked, 1);
        assert_eq!(
            dirty.require_safe_update().unwrap_err().code,
            "GIT_WORKTREE_DIRTY"
        );
    }

    #[test]
    fn integration_exact_detached_worktree_and_guarded_cleanup() {
        if !git_available() {
            return;
        }
        let repo = repository();
        let client = GitClient::open(repo.path()).unwrap();
        let commit = client.inspect().unwrap().head;
        let cache = tempfile::tempdir().unwrap();
        let receipt = client
            .create_build_worktree(&commit, cache.path(), "build-one")
            .unwrap();
        assert_eq!(
            GitClient::open(&receipt.path)
                .unwrap()
                .inspect()
                .unwrap()
                .head,
            commit
        );
        assert_eq!(
            client
                .cleanup_build_worktree(&receipt, false)
                .unwrap_err()
                .code,
            "GIT_WORKTREE_CLEANUP_BLOCKED"
        );
        let cleanup = client.cleanup_build_worktree(&receipt, true).unwrap();
        assert!(cleanup.removed);
        assert!(!receipt.path.exists());
    }

    #[test]
    fn remote_transport_allowlist_rejects_helpers_credentials_and_plain_http() {
        assert!(validate_remote_url("git@github.com:org/repo.git").is_ok());
        assert!(validate_remote_url("ssh://git@github.com/org/repo.git").is_ok());
        assert!(validate_remote_url("https://github.com/org/repo.git").is_ok());
        assert!(validate_remote_url("../local-mirror.git").is_ok());
        for rejected in [
            "ext::sh -c evil",
            "hg::https://example.invalid/repo",
            "http://example.invalid/repo",
            "https://token@example.com/org/repo.git",
            "https://user:pass@example.com/org/repo.git",
            "ssh://user:secret@example.invalid/repository",
            "-oProxyCommand=evil:repo",
        ] {
            assert_eq!(
                validate_remote_url(rejected).unwrap_err().code,
                "GIT_REPOSITORY_UNTRUSTED"
            );
        }
    }

    #[test]
    fn dangerous_repository_local_git_configuration_is_classified() {
        for key in [
            "credential.helper",
            "credential.https://github.com.helper",
            "core.sshCommand",
            "filter.evil.smudge",
            "http.proxy",
            "protocol.ext.allow",
            "url.ext::evil.insteadOf",
            "include.path",
            "includeIf.gitdir:test.path",
            "remote.origin.uploadpack",
        ] {
            assert!(dangerous_local_git_key(key), "{key}");
        }
        for key in [
            "remote.origin.url",
            "remote.origin.fetch",
            "branch.main.merge",
        ] {
            assert!(!dangerous_local_git_key(key), "{key}");
        }
    }

    #[test]
    fn fetch_rejects_repository_local_executable_configuration() {
        if !git_available() {
            return;
        }
        let repo = repository();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(
            repo.path(),
            &["config", "credential.helper", "!malicious-helper"],
        );
        let error = GitClient::open(repo.path())
            .unwrap()
            .fetch("origin", FetchOptions::default())
            .unwrap_err();
        assert_eq!(error.code, "GIT_REPOSITORY_UNTRUSTED");
    }

    #[test]
    fn checkout_filter_attributes_are_blocked_conservatively() {
        assert!(attributes_enable_filter("*.rbxm filter=lfs -text\n"));
        assert!(attributes_enable_filter("*.bin -filter\n"));
        assert!(!attributes_enable_filter(
            "*.luau text eol=lf\n# filter=ignored\n"
        ));
    }

    #[test]
    fn integration_fetch_then_fast_forward_is_explicit() {
        if !git_available() {
            return;
        }
        let repo = repository();
        let branch = GitClient::open(repo.path())
            .unwrap()
            .inspect()
            .unwrap()
            .branch
            .unwrap();
        let remote_parent = tempfile::tempdir().unwrap();
        let bare = remote_parent.path().join("remote.git");
        fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare", "-q"]);
        git(
            repo.path(),
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        git(repo.path(), &["push", "-qu", "origin", &branch]);

        let peer = remote_parent.path().join("peer");
        let output = Command::new("git")
            .args([
                "clone",
                "-q",
                bare.to_str().unwrap(),
                peer.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        git(&peer, &["config", "user.email", "peer@example.invalid"]);
        git(&peer, &["config", "user.name", "NeuMan Peer"]);
        fs::write(peer.join("peer.txt"), b"remote change").unwrap();
        git(&peer, &["add", "peer.txt"]);
        git(&peer, &["commit", "-qm", "peer change"]);
        git(&peer, &["push", "-q", "origin", &branch]);

        let client = GitClient::open(repo.path()).unwrap();
        let before = client.inspect().unwrap().head;
        let fetch = client.fetch("origin", FetchOptions::default()).unwrap();
        assert!(!fetch.changed_refs.is_empty());
        let upstream = format!("origin/{branch}");
        let update = client.update_fast_forward(&upstream).unwrap();
        assert_eq!(update.before, before);
        assert_ne!(update.after, before);
        assert!(client.inspect().unwrap().is_clean());
    }

    #[test]
    fn fast_forward_rejects_target_checkout_filters_before_mutation() {
        if !git_available() {
            return;
        }
        let repo = repository();
        let branch = GitClient::open(repo.path())
            .unwrap()
            .inspect()
            .unwrap()
            .branch
            .unwrap();
        let remote_parent = tempfile::tempdir().unwrap();
        let bare = remote_parent.path().join("remote.git");
        fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare", "-q"]);
        git(
            repo.path(),
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        git(repo.path(), &["push", "-qu", "origin", &branch]);

        let peer = remote_parent.path().join("peer-filter");
        let output = Command::new("git")
            .args([
                "clone",
                "-q",
                bare.to_str().unwrap(),
                peer.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        git(&peer, &["config", "user.email", "peer@example.invalid"]);
        git(&peer, &["config", "user.name", "NeuMan Peer"]);
        fs::write(peer.join(".gitattributes"), b"*.luau filter=evil\n").unwrap();
        fs::write(peer.join("payload.luau"), b"return true\n").unwrap();
        git(&peer, &["add", ".gitattributes", "payload.luau"]);
        git(&peer, &["commit", "-qm", "unsafe checkout filter"]);
        git(&peer, &["push", "-q", "origin", &branch]);

        let client = GitClient::open(repo.path()).unwrap();
        let before = client.inspect().unwrap().head;
        client.fetch("origin", FetchOptions::default()).unwrap();
        let error = client
            .update_fast_forward(&format!("origin/{branch}"))
            .unwrap_err();
        assert_eq!(error.code, "GIT_REPOSITORY_UNTRUSTED");
        assert_eq!(client.inspect().unwrap().head, before);
        assert!(!repo.path().join("payload.luau").exists());
    }

    #[test]
    fn live_session_manager_owns_one_child_and_reports_lifecycle() {
        let tools = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let pin = fake_rojo(tools.path());
        let project_name = "default.project.json";
        let project_path = workspace.path().join(project_name);
        fs::write(&project_path, b"{\"name\":\"live\"}").unwrap();
        let request = session_request(&pin, workspace.path(), project_name, "lobby");
        let range = RojoPortRange::create(41_000, 41_031).unwrap();
        let mut manager = RojoSessionManager::new(range, ProcessLimits::default());

        let created = manager.start(&request).unwrap();
        assert!(created.created);
        let key = created.status.key.clone();
        let healthy = wait_for_session_state(&mut manager, &key, RojoServerState::Healthy);
        assert!(healthy.pid.is_some());
        assert_eq!(healthy.rojo_version, "7.7.1");
        assert_eq!(healthy.rojo_sha256, pin.sha256);

        let duplicate = manager.start(&request).unwrap();
        assert!(!duplicate.created);
        assert_eq!(duplicate.status.session_id, healthy.session_id);
        assert_eq!(duplicate.status.context_id, healthy.context_id);
        assert_eq!(duplicate.status.port, healthy.port);
        assert_eq!(duplicate.status.pid, healthy.pid);

        fs::write(&project_path, b"{\"name\":\"changed\"}").unwrap();
        assert_eq!(
            manager.start(&request).unwrap_err().code,
            "GIT_ROJO_SESSION_CONFLICT"
        );
        fs::write(&project_path, b"{\"name\":\"live\"}").unwrap();

        let stopped = manager.stop(&key).unwrap();
        assert_eq!(stopped.state, RojoServerState::Stopped);
        assert!(stopped.pid.is_none());
        assert_eq!(
            stopped.last_exit.as_ref().unwrap().kind,
            RojoChildExitKind::RequestedStop
        );
        assert_eq!(manager.list().unwrap().len(), 1);

        let restarted = manager.restart(&key).unwrap();
        assert_eq!(restarted.restart_attempts, 1);
        let healthy_again = wait_for_session_state(&mut manager, &key, RojoServerState::Healthy);
        assert_eq!(healthy_again.port, healthy.port);
        assert_eq!(healthy_again.context_id, healthy.context_id);

        manager.stop(&key).unwrap();
        assert!(manager.remove_inactive(&key).unwrap());
        assert!(manager.list().unwrap().is_empty());

        let recreated = manager.start(&request).unwrap();
        assert_eq!(recreated.status.port, healthy.port);
        assert_eq!(recreated.status.session_id, healthy.session_id);
        assert_eq!(recreated.status.context_id, healthy.context_id);
        manager.stop_all().unwrap();
    }

    #[test]
    fn live_session_manager_reports_unexpected_child_exit() {
        let tools = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let pin = fake_rojo(tools.path());
        fs::write(
            workspace.path().join("crash.project.json"),
            b"{\"exitImmediately\":true}",
        )
        .unwrap();
        let request = session_request(&pin, workspace.path(), "crash.project.json", "crash-test");
        let mut manager = RojoSessionManager::new(
            RojoPortRange::create(41_100, 41_131).unwrap(),
            ProcessLimits::default(),
        );
        let outcome = manager.start(&request).unwrap();
        let status =
            wait_for_session_state(&mut manager, &outcome.status.key, RojoServerState::Exited);
        let exit = status.last_exit.unwrap();
        assert_eq!(exit.kind, RojoChildExitKind::Unexpected);
        assert_eq!(exit.exit_code, Some(17));
        assert!(status.stderr.contains("intentional fake crash"));
        assert!(manager.remove_inactive(&outcome.status.key).unwrap());
    }

    #[test]
    fn live_session_port_and_key_validation_is_fail_closed() {
        assert!(RojoPortRange::create(80, 90).is_err());
        assert!(RojoPortRange::create(42_000, 41_999).is_err());
        assert!(serde_json::from_str::<RojoPortRange>(r#"{"start":80,"end":90}"#).is_err());
        let range = RojoPortRange::create(42_000, 42_010).unwrap();
        assert_eq!(range.start(), 42_000);
        assert_eq!(range.end(), 42_010);
        let workspace = tempfile::tempdir().unwrap();
        assert!(RojoSessionKey::create(workspace.path(), "../other").is_err());
        let key = RojoSessionKey::create(workspace.path(), "lobby").unwrap();
        assert_eq!(
            key.workspace_root(),
            fs::canonicalize(workspace.path()).unwrap()
        );
        assert_eq!(key.place_key(), "lobby");
    }
}
