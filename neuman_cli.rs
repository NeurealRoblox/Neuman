//! `neuman` command-line vertical slice.
//!
//! Every command supports a single versioned JSON result on stdout. Provider
//! secrets are accepted only through an operator-controlled environment variable
//! and are never echoed.

use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::{Args, Parser, Subcommand};
use neuman::core::{
    CapturedCell, ContentStore, CoreError, Ledger, OperatorApiKey, PreflightEvidence,
    ProjectManifest, ReleaseRecord, RobloxPlacePublisher, capture_art_revision,
    create_build_bundle, git_status, release_preflight, starter_manifest, validate_git_commit,
};
use neuman::domain::{
    ArtRevisionId, BuildRepositoryIdentity, ContentHash, GitOid, LogicalBuildInput, ProjectId,
    ReleaseId, ReleaseStatus, Sha256Hash,
};
use neuman::git_rojo::{FetchOptions, GitClient, TagPolicy};
use neuman::{core, domain};
use serde::Serialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Parser)]
#[command(
    name = "neuman",
    version,
    about = "Deterministic Roblox project, art, build, and release manager"
)]
struct Cli {
    /// Emit one versioned JSON result object to stdout.
    #[arg(long, global = true)]
    json: bool,
    /// Project directory or a child path.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a safe local project without credentials or production targets.
    Init(InitArgs),
    /// Parse and validate the effective project manifest.
    Validate,
    /// Project-oriented aliases used by desktop integrations.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Show project, Git, art, CAS, and local ledger status.
    Status,
    /// Inspect or explicitly synchronize the Git code authority.
    Code {
        #[command(subcommand)]
        command: CodeCommand,
    },
    /// Inspect the local authenticated Studio bridge.
    Bridge {
        #[command(subcommand)]
        command: BridgeCommand,
    },
    /// Capture and inspect immutable art revisions.
    Art {
        #[command(subcommand)]
        command: ArtCommand,
    },
    /// Create deterministic logical builds and release bundles.
    Build {
        #[command(subcommand)]
        command: BuildCommand,
    },
    /// Create, preflight, publish, and inspect releases.
    Release {
        #[command(subcommand)]
        command: ReleaseCommand,
    },
}

#[derive(Subcommand)]
enum CodeCommand {
    /// Probe the qualified system Git and repository capabilities.
    Probe,
    /// Inspect exact branch, worktree, conflict, and operation state.
    Status,
    /// Fetch one configured remote without changing the worktree.
    Fetch(CodeFetchArgs),
    /// Fetch, then update the attached clean branch by fast-forward only.
    Sync(CodeSyncArgs),
    /// Update from an already-fetched upstream by fast-forward only.
    Update(CodeUpdateArgs),
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum TagsArg {
    Auto,
    All,
    None,
}

impl From<TagsArg> for TagPolicy {
    fn from(value: TagsArg) -> Self {
        match value {
            TagsArg::Auto => Self::Auto,
            TagsArg::All => Self::All,
            TagsArg::None => Self::None,
        }
    }
}

#[derive(Args)]
struct CodeFetchArgs {
    /// Configured remote name. URLs are deliberately not accepted.
    #[arg(long, default_value = "origin")]
    remote: String,
    /// Explicitly prune deleted remote-tracking refs.
    #[arg(long)]
    prune: bool,
    /// Tag fetch policy.
    #[arg(long, value_enum, default_value = "auto")]
    tags: TagsArg,
}

#[derive(Args)]
struct CodeSyncArgs {
    /// Configured remote name. URLs are deliberately not accepted.
    #[arg(long, default_value = "origin")]
    remote: String,
    /// Exact upstream ref; defaults to REMOTE/project.defaultBranch.
    #[arg(long)]
    upstream: Option<String>,
    /// Explicitly prune deleted remote-tracking refs.
    #[arg(long)]
    prune: bool,
    /// Tag fetch policy.
    #[arg(long, value_enum, default_value = "auto")]
    tags: TagsArg,
}

#[derive(Args)]
struct CodeUpdateArgs {
    /// Already-fetched upstream ref such as origin/main.
    upstream: String,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Parse and validate the effective manifest.
    Validate,
}

#[derive(Subcommand)]
enum BridgeCommand {
    /// Report bridge discovery state. The bridge process owns live session detail.
    Status,
}

#[derive(Args)]
struct InitArgs {
    /// Project slug.
    #[arg(long)]
    slug: String,
    /// Display name.
    #[arg(long)]
    name: String,
    /// Refuse a non-empty directory unless this is set.
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
enum ArtCommand {
    /// List locally recorded revisions.
    Status,
    /// Capture native RBXM cells as a proposed or accepted local revision.
    Capture(ArtCaptureArgs),
    /// Show immutable metadata for one revision.
    Show { revision: String },
}

#[derive(Args)]
struct ArtCaptureArgs {
    /// Art channel key.
    #[arg(long)]
    channel: Option<String>,
    /// `DataModel/Slot=path/to/cell.rbxm`; repeat for multiple cells.
    #[arg(long = "cell", required = true)]
    cells: Vec<String>,
    /// Human revision message.
    #[arg(long)]
    message: String,
    /// Stable author/principal label.
    #[arg(long, default_value = "local-user")]
    author: String,
    /// Parent revision IDs.
    #[arg(long = "parent")]
    parents: Vec<String>,
    /// Mark accepted. Protected channels reject local acceptance.
    #[arg(long)]
    accept: bool,
}

#[derive(Subcommand)]
enum BuildCommand {
    /// Resolve exact inputs and create an environment-neutral immutable bundle.
    Create(BuildCreateArgs),
}

#[derive(Args)]
struct BuildCreateArgs {
    /// Place key.
    #[arg(long)]
    place: Option<String>,
    /// Exact Git commit; defaults to HEAD.
    #[arg(long)]
    code: Option<String>,
    /// Accepted art revision.
    #[arg(long)]
    art: String,
    /// Assembled candidate RBXL. Defaults to the configured base template.
    #[arg(long)]
    candidate: Option<PathBuf>,
}

#[derive(Subcommand)]
enum ReleaseCommand {
    /// Create an immutable release request for a manifest target.
    #[command(alias = "plan")]
    Create(ReleaseCreateArgs),
    /// Evaluate and store immediate fail-closed preflight evidence.
    Preflight(ReleasePreflightArgs),
    /// Re-run preflight, then publish with an operator environment secret.
    Publish(ReleasePublishArgs),
    /// Show a durable local release record.
    Status { release: String },
}

#[derive(Args)]
struct ReleaseCreateArgs {
    /// Immutable release bundle hash.
    #[arg(long)]
    bundle: String,
    /// Environment key.
    #[arg(long)]
    environment: String,
    /// Place key.
    #[arg(long)]
    place: Option<String>,
    /// Requesting principal label.
    #[arg(long, default_value = "local-operator")]
    requested_by: String,
    /// Record required approval as already satisfied (local/operator mode).
    #[arg(long)]
    approved: bool,
}

#[derive(Args)]
struct ReleasePreflightArgs {
    /// Release ID.
    release: String,
    /// Exact target permission was verified.
    #[arg(long)]
    permission_verified: bool,
    /// Expected predecessor matches current provider observation.
    #[arg(long)]
    predecessor_matches: bool,
    /// Drift classification; only `clean` passes.
    #[arg(long, default_value = "unknown")]
    drift: String,
    /// Per-target release lease is held.
    #[arg(long)]
    lease_held: bool,
    /// Exact-bundle staging proof is valid.
    #[arg(long)]
    staging_proof_valid: bool,
}

#[derive(Args)]
struct ReleasePublishArgs {
    /// Release ID.
    release: String,
    /// Exact target permission was verified immediately before mutation.
    #[arg(long)]
    permission_verified: bool,
    /// Expected predecessor matches current provider observation.
    #[arg(long)]
    predecessor_matches: bool,
    /// Must be `clean`; unknown never passes.
    #[arg(long, default_value = "unknown")]
    drift: String,
    /// Per-target release lease is held.
    #[arg(long)]
    lease_held: bool,
    /// Exact-bundle staging proof is valid.
    #[arg(long)]
    staging_proof_valid: bool,
    /// Environment variable containing the operator-managed API key.
    #[arg(long, default_value = "NEUMAN_ROBLOX_OPERATOR_API_KEY")]
    operator_key_env: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<T: Serialize> {
    schema_version: &'static str,
    ok: bool,
    result: Option<T>,
    error: Option<ErrorBody>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: String,
    retryable: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json_output = cli.json;
    match execute(cli).await {
        Ok(result) => {
            let envelope = Envelope {
                schema_version: "1.0",
                ok: true,
                result: Some(result.clone()),
                error: None,
            };
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("serializable result")
                );
            } else {
                print_human(&result);
            }
        }
        Err(error) => {
            let envelope = Envelope::<Value> {
                schema_version: "1.0",
                ok: false,
                result: None,
                error: Some(ErrorBody {
                    code: error.code,
                    message: error.message.clone(),
                    retryable: error.retryable,
                }),
            };
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("serializable error")
                );
            } else {
                eprintln!("{}: {}", error.code, error.message);
            }
            std::process::exit(exit_code(error.code));
        }
    }
}

async fn execute(cli: Cli) -> Result<Value, CoreError> {
    let Cli {
        root: selected_root,
        command,
        ..
    } = cli;
    let command = match command {
        Command::Init(args) => return initialize(&selected_root, args),
        other => other,
    };
    let root = core::discover_project(&selected_root)?;
    let (manifest, validation) = ProjectManifest::load(&root).map_err(report_error)?;
    let ledger = Ledger::open(root.join(".neuman/state.sqlite3"))?;
    let store = ContentStore::open(root.join(".neuman/cas"))?;
    let project_id = read_project_id(&root)?;
    match command {
        Command::Init(_) => unreachable!(),
        Command::Validate => Ok(serde_json::to_value(validation).expect("serializable validation")),
        Command::Project {
            command: ProjectCommand::Validate,
        } => Ok(serde_json::to_value(validation).expect("serializable validation")),
        Command::Status => project_status(&root, &manifest, &ledger, validation.manifest_hash),
        Command::Code { command } => code_command(&root, &manifest, command),
        Command::Bridge {
            command: BridgeCommand::Status,
        } => Ok(
            json!({"state":"not-running","sessions":[],"note":"live bridge sessions are owned by the neuman-bridge process"}),
        ),
        Command::Art { command } => {
            art_command(&root, &manifest, &ledger, &store, project_id, command)
        }
        Command::Build { command } => {
            build_command(&root, &manifest, &ledger, &store, project_id, command)
        }
        Command::Release { command } => {
            release_command(&root, &manifest, &ledger, &store, project_id, command).await
        }
    }
}

fn code_command(
    root: &Path,
    manifest: &ProjectManifest,
    command: CodeCommand,
) -> Result<Value, CoreError> {
    let git = GitClient::open(root).map_err(integration_error)?;
    match command {
        CodeCommand::Probe => serde_json::to_value(git.probe().map_err(integration_error)?)
            .map_err(|error| CoreError::new("GIT_RECEIPT_SERIALIZE_FAILED", error.to_string())),
        CodeCommand::Status => serde_json::to_value(git.inspect().map_err(integration_error)?)
            .map_err(|error| CoreError::new("GIT_RECEIPT_SERIALIZE_FAILED", error.to_string())),
        CodeCommand::Fetch(args) => {
            let receipt = git
                .fetch(
                    &args.remote,
                    FetchOptions {
                        prune: args.prune,
                        tags: args.tags.into(),
                    },
                )
                .map_err(integration_error)?;
            serde_json::to_value(receipt)
                .map_err(|error| CoreError::new("GIT_RECEIPT_SERIALIZE_FAILED", error.to_string()))
        }
        CodeCommand::Sync(args) => {
            let upstream = args.upstream.unwrap_or_else(|| {
                format!("{}/{}", args.remote, manifest.repository.default_branch)
            });
            let fetch = git
                .fetch(
                    &args.remote,
                    FetchOptions {
                        prune: args.prune,
                        tags: args.tags.into(),
                    },
                )
                .map_err(integration_error)?;
            let update = git
                .update_fast_forward(&upstream)
                .map_err(integration_error)?;
            Ok(json!({"fetch":fetch,"update":update}))
        }
        CodeCommand::Update(args) => serde_json::to_value(
            git.update_fast_forward(&args.upstream)
                .map_err(integration_error)?,
        )
        .map_err(|error| CoreError::new("GIT_RECEIPT_SERIALIZE_FAILED", error.to_string())),
    }
}

fn integration_error(error: neuman::git_rojo::IntegrationError) -> CoreError {
    CoreError::new(error.code, error.message)
}

fn initialize(root: &Path, args: InitArgs) -> Result<Value, CoreError> {
    fs::create_dir_all(root)
        .map_err(|error| CoreError::new("INIT_DIRECTORY_FAILED", error.to_string()))?;
    if root.join("neuman.project.yaml").exists() && !args.force {
        return Err(CoreError::new(
            "PROJECT_ALREADY_EXISTS",
            "neuman.project.yaml already exists; pass --force to replace only the starter files",
        ));
    }
    let manifest = starter_manifest(&args.slug, &args.name)?;
    ProjectManifest::parse(manifest.as_bytes()).map_err(report_error)?;
    fs::create_dir_all(root.join(".neuman/cas"))
        .map_err(|error| CoreError::new("INIT_DIRECTORY_FAILED", error.to_string()))?;
    fs::create_dir_all(root.join("places"))
        .map_err(|error| CoreError::new("INIT_DIRECTORY_FAILED", error.to_string()))?;
    atomic_write(&root.join("neuman.project.yaml"), manifest.as_bytes())?;
    let project_id = ProjectId::new();
    atomic_write(
        &root.join(".neuman/project-id"),
        project_id.to_string().as_bytes(),
    )?;
    let gitignore = root.join(".gitignore");
    let mut ignore = fs::read_to_string(&gitignore).unwrap_or_default();
    if !ignore
        .lines()
        .any(|line| line.trim() == ".neuman/local.json")
    {
        ignore.push_str("\n.neuman/local.json\n.neuman/state.sqlite3*\n.neuman/cas/\n");
        atomic_write(&gitignore, ignore.as_bytes())?;
    }
    Ledger::open(root.join(".neuman/state.sqlite3"))?;
    ContentStore::open(root.join(".neuman/cas"))?;
    Ok(
        json!({"projectId":project_id,"root":root,"manifest":"neuman.project.yaml","next":["configure Roblox targets", "capture an accepted art revision", "build an immutable bundle"]}),
    )
}

fn project_status(
    root: &Path,
    manifest: &ProjectManifest,
    ledger: &Ledger,
    manifest_hash: Option<ContentHash>,
) -> Result<Value, CoreError> {
    let git = if root.join(".git").exists() {
        match git_status(root, manifest.repository.object_format) {
            Ok(status) => serde_json::to_value(status).unwrap_or(Value::Null),
            Err(error) => {
                json!({"available":false,"error":{"code":error.code,"message":error.message}})
            }
        }
    } else {
        json!({"available":false,"reason":"not-a-git-worktree"})
    };
    let revisions = ledger.art_revisions()?;
    Ok(
        json!({"project":{"slug":manifest.project.slug,"displayName":manifest.project.display_name,"manifestHash":manifest_hash},"git":git,"art":{"revisionCount":revisions.len(),"latest":revisions.first()},"localLedger":true,"cas":true}),
    )
}

fn art_command(
    root: &Path,
    manifest: &ProjectManifest,
    ledger: &Ledger,
    store: &ContentStore,
    project_id: ProjectId,
    command: ArtCommand,
) -> Result<Value, CoreError> {
    match command {
        ArtCommand::Status => Ok(json!({"revisions":ledger.art_revisions()?})),
        ArtCommand::Show { revision } => {
            let id = parse_art_id(&revision)?;
            let revision = ledger
                .art_revision(id)?
                .ok_or_else(|| CoreError::new("ART_REVISION_NOT_FOUND", id.to_string()))?;
            Ok(serde_json::to_value(revision).expect("serializable revision"))
        }
        ArtCommand::Capture(args) => {
            let channel = args
                .channel
                .or_else(|| manifest.project.default_art_channel.clone())
                .ok_or_else(|| {
                    CoreError::new(
                        "ART_CHANNEL_REQUIRED",
                        "pass --channel or configure project.defaultArtChannel",
                    )
                })?;
            let channel_config = manifest
                .art_channels
                .get(&channel)
                .ok_or_else(|| CoreError::new("ART_CHANNEL_UNKNOWN", &channel))?;
            if args.accept && channel_config.protected {
                return Err(CoreError::new(
                    "ART_PROTECTED_LOCAL_ACCEPTANCE",
                    "protected channels require the Hub approval ledger",
                ));
            }
            let parents = args
                .parents
                .iter()
                .map(|value| parse_art_id(value))
                .collect::<Result<Vec<_>, _>>()?;
            let mut cells = Vec::new();
            for cell in args.cells {
                let (slot, path) = cell.split_once('=').ok_or_else(|| {
                    CoreError::new(
                        "ART_CELL_ARGUMENT_INVALID",
                        "expected DataModel/Slot=path/to/cell.rbxm",
                    )
                })?;
                let file = root.join(path);
                let bytes = fs::read(&file).map_err(|error| {
                    CoreError::new(
                        "ART_CELL_READ_FAILED",
                        format!("{}: {error}", file.display()),
                    )
                })?;
                cells.push(CapturedCell {
                    cell_id: domain::CellId::new(),
                    slot_path: if slot.starts_with('/') {
                        slot.into()
                    } else {
                        format!("/{slot}")
                    },
                    bytes,
                });
            }
            let revision = capture_art_revision(
                store,
                ledger,
                project_id,
                channel,
                parents,
                cells,
                args.author,
                args.message,
                now()?,
                args.accept,
            )?;
            Ok(serde_json::to_value(revision).expect("serializable revision"))
        }
    }
}

fn build_command(
    root: &Path,
    manifest: &ProjectManifest,
    ledger: &Ledger,
    store: &ContentStore,
    project_id: ProjectId,
    command: BuildCommand,
) -> Result<Value, CoreError> {
    match command {
        BuildCommand::Create(args) => {
            let place_key = args
                .place
                .or_else(|| manifest.project.default_place.clone())
                .ok_or_else(|| {
                    CoreError::new(
                        "BUILD_PLACE_REQUIRED",
                        "pass --place or configure project.defaultPlace",
                    )
                })?;
            let place = manifest
                .places
                .get(&place_key)
                .ok_or_else(|| CoreError::new("BUILD_PLACE_UNKNOWN", &place_key))?;
            let art_id = parse_art_id(&args.art)?;
            let art = ledger
                .art_revision(art_id)?
                .ok_or_else(|| CoreError::new("BUILD_ART_NOT_FOUND", art_id.to_string()))?;
            let code_oid = if let Some(value) = args.code {
                GitOid::parse_for(&value, manifest.repository.object_format)
                    .map_err(|error| CoreError::new("GIT_OID_INVALID", error.to_string()))?
            } else {
                git_status(root, manifest.repository.object_format)?.head
            };
            if root.join(".git").exists() {
                validate_git_commit(root, &code_oid)?;
                let status = git_status(root, manifest.repository.object_format)?;
                if manifest.repository.require_clean_worktree_for_build && !status.clean {
                    return Err(CoreError::new(
                        "GIT_WORKTREE_DIRTY",
                        "commit or stash all tracked and untracked files before building",
                    ));
                }
            }
            let base_path = root.join(&place.base_template.path);
            let base_bytes = fs::read(&base_path).map_err(|error| {
                CoreError::new(
                    "BUILD_BASE_TEMPLATE_READ_FAILED",
                    format!("{}: {error}", base_path.display()),
                )
            })?;
            if let Some(expected) = place.base_template.sha256 {
                let observed = Sha256Hash::digest(&base_bytes);
                if observed != expected {
                    return Err(CoreError::new(
                        "BUILD_BASE_TEMPLATE_HASH_MISMATCH",
                        format!("expected {expected}, observed {observed}"),
                    ));
                }
            }
            let candidate_path = args
                .candidate
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        root.join(path)
                    }
                })
                .unwrap_or_else(|| base_path.clone());
            let candidate = fs::read(&candidate_path).map_err(|error| {
                CoreError::new(
                    "BUILD_CANDIDATE_READ_FAILED",
                    format!("{}: {error}", candidate_path.display()),
                )
            })?;
            let manifest_hash = domain::hash_canonical("neuman-project-manifest-v1\0", manifest)
                .map_err(|error| CoreError::new("BUILD_MANIFEST_HASH_FAILED", error.to_string()))?;
            let lock_bytes = fs::read(root.join("neuman.lock.json")).unwrap_or_else(|_| {
                domain::canonical_json(&manifest.toolchain).unwrap_or_default()
            });
            let repository_id = manifest
                .repository
                .github_repository_id
                .as_ref()
                .map(|id| format!("github:{id}"))
                .or_else(|| manifest.repository.remote.clone())
                .unwrap_or_else(|| format!("local:{}", manifest.project.slug));
            let input = LogicalBuildInput {
                schema_version: "1.0".into(),
                project_id,
                place_key: place_key.clone(),
                repository: BuildRepositoryIdentity {
                    id: repository_id,
                    object_format: manifest.repository.object_format,
                },
                code_commit: code_oid,
                art_revision_id: art_id,
                art_state_root_hash: art.state_root_hash,
                base_template_hash: ContentHash::digest(&base_bytes),
                dependency_manifest_hash: domain::hash_canonical(
                    "neuman-dependencies-v1\0",
                    &json!({}),
                )
                .map_err(|error| {
                    CoreError::new("BUILD_DEPENDENCY_HASH_FAILED", error.to_string())
                })?,
                toolchain_lock_hash: ContentHash::digest(&lock_bytes),
                policy_revision_hash: domain::hash_canonical(
                    "neuman-policy-v1\0",
                    manifest
                        .policies
                        .get(&place.release_policy)
                        .expect("validated policy"),
                )
                .map_err(|error| CoreError::new("BUILD_POLICY_HASH_FAILED", error.to_string()))?,
                manifest_hash,
                profile: "release".into(),
            };
            Ok(serde_json::to_value(create_build_bundle(
                manifest,
                ledger,
                store,
                input,
                &candidate,
                &now()?,
            )?)
            .expect("serializable build"))
        }
    }
}

async fn release_command(
    _root: &Path,
    manifest: &ProjectManifest,
    ledger: &Ledger,
    store: &ContentStore,
    project_id: ProjectId,
    command: ReleaseCommand,
) -> Result<Value, CoreError> {
    match command {
        ReleaseCommand::Create(args) => {
            let bundle_hash = parse_hash(&args.bundle)?;
            let bundle = ledger.bundle(bundle_hash)?.ok_or_else(|| {
                CoreError::new("RELEASE_BUNDLE_NOT_FOUND", bundle_hash.to_string())
            })?;
            let place_key = args
                .place
                .or_else(|| manifest.project.default_place.clone())
                .ok_or_else(|| {
                    CoreError::new(
                        "RELEASE_PLACE_REQUIRED",
                        "pass --place or configure defaultPlace",
                    )
                })?;
            if bundle.place_key != place_key {
                return Err(CoreError::new(
                    "RELEASE_BUNDLE_PLACE_MISMATCH",
                    "bundle was built for a different place",
                ));
            }
            let place = manifest
                .places
                .get(&place_key)
                .ok_or_else(|| CoreError::new("RELEASE_PLACE_UNKNOWN", &place_key))?;
            let target = place.targets.get(&args.environment).ok_or_else(|| {
                CoreError::new(
                    "RELEASE_TARGET_MISSING",
                    "place has no target for this environment",
                )
            })?;
            let status = if args.approved {
                ReleaseStatus::Approved
            } else {
                ReleaseStatus::AwaitingApproval
            };
            let release = ReleaseRecord {
                release_id: ReleaseId::new(),
                project_id,
                bundle_hash,
                environment: args.environment,
                place_key,
                target: core::RobloxTarget {
                    universe_id: target.universe_id.clone(),
                    place_id: target.place_id.clone(),
                    creator: target.creator.clone(),
                },
                status,
                requested_by: args.requested_by,
                created_at: now()?,
            };
            ledger.put_release(&release)?;
            Ok(serde_json::to_value(release).expect("serializable release"))
        }
        ReleaseCommand::Status { release } => {
            let id = parse_release_id(&release)?;
            let record = ledger
                .release(id)?
                .ok_or_else(|| CoreError::new("RELEASE_NOT_FOUND", id.to_string()))?;
            Ok(serde_json::to_value(record).expect("serializable release"))
        }
        ReleaseCommand::Preflight(args) => {
            let id = parse_release_id(&args.release)?;
            let mut record = ledger
                .release(id)?
                .ok_or_else(|| CoreError::new("RELEASE_NOT_FOUND", id.to_string()))?;
            if record.status == ReleaseStatus::Approved {
                ledger.transition_release(
                    id,
                    ReleaseStatus::Approved,
                    ReleaseStatus::Preflighting,
                )?;
                record.status = ReleaseStatus::Preflighting;
            }
            let receipt = evaluate_preflight(
                ledger,
                store,
                &record,
                args.permission_verified,
                args.predecessor_matches,
                args.drift,
                args.lease_held,
                args.staging_proof_valid,
            )?;
            Ok(serde_json::to_value(receipt).expect("serializable preflight"))
        }
        ReleaseCommand::Publish(args) => {
            let id = parse_release_id(&args.release)?;
            let mut record = ledger
                .release(id)?
                .ok_or_else(|| CoreError::new("RELEASE_NOT_FOUND", id.to_string()))?;
            if record.status == ReleaseStatus::Approved {
                ledger.transition_release(
                    id,
                    ReleaseStatus::Approved,
                    ReleaseStatus::Preflighting,
                )?;
                record.status = ReleaseStatus::Preflighting;
            }
            let receipt = evaluate_preflight(
                ledger,
                store,
                &record,
                args.permission_verified,
                args.predecessor_matches,
                args.drift,
                args.lease_held,
                args.staging_proof_valid,
            )?;
            if !receipt.passed {
                return Err(CoreError::new(
                    "RELEASE_PREFLIGHT_FAILED",
                    receipt.failed_gates.join(", "),
                ));
            }
            let bundle = ledger.bundle(record.bundle_hash)?.ok_or_else(|| {
                CoreError::new("RELEASE_BUNDLE_NOT_FOUND", record.bundle_hash.to_string())
            })?;
            let artifact = bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.name == "place-candidate")
                .ok_or_else(|| {
                    CoreError::new(
                        "RELEASE_ARTIFACT_MISSING",
                        "bundle has no place-candidate artifact",
                    )
                })?;
            let bytes = store.get(artifact.content_hash)?;
            let secret = std::env::var(&args.operator_key_env).map_err(|_| {
                CoreError::new(
                    "ROBLOX_OPERATOR_KEY_MISSING",
                    format!(
                        "operator secret environment variable {} is not set",
                        args.operator_key_env
                    ),
                )
            })?;
            let publisher = RobloxPlacePublisher::new(OperatorApiKey::parse(secret)?)?;
            ledger.transition_release(
                id,
                ReleaseStatus::Preflighting,
                ReleaseStatus::Publishing,
            )?;
            match publisher
                .publish(&record.target.universe_id, &record.target.place_id, &bytes)
                .await
            {
                Ok(publish_receipt) => {
                    let created_at = now()?;
                    ledger.put_receipt(
                        id,
                        "publish",
                        &format!("publish:{}", publish_receipt.artifact_hash),
                        &publish_receipt,
                        &created_at,
                    )?;
                    ledger.transition_release(
                        id,
                        ReleaseStatus::Publishing,
                        ReleaseStatus::Published,
                    )?;
                    Ok(
                        json!({"releaseId":id,"status":"published","preflight":receipt,"publication":publish_receipt}),
                    )
                }
                Err(error) => {
                    if error.retryable {
                        let _ = ledger.transition_release(
                            id,
                            ReleaseStatus::Publishing,
                            ReleaseStatus::UnknownExternalState,
                        );
                    } else {
                        let _ = ledger.transition_release(
                            id,
                            ReleaseStatus::Publishing,
                            ReleaseStatus::FailedNoChange,
                        );
                    }
                    Err(error)
                }
            }
        }
    }
}

fn evaluate_preflight(
    ledger: &Ledger,
    store: &ContentStore,
    record: &ReleaseRecord,
    permission: bool,
    predecessor: bool,
    drift: String,
    lease: bool,
    staging: bool,
) -> Result<core::PreflightReceipt, CoreError> {
    let bundle = ledger.bundle(record.bundle_hash)?.ok_or_else(|| {
        CoreError::new("RELEASE_BUNDLE_NOT_FOUND", record.bundle_hash.to_string())
    })?;
    let mut bundle_verified = bundle
        .bundle_hash()
        .map_err(|error| CoreError::new("BUNDLE_HASH_FAILED", error.to_string()))?
        == record.bundle_hash;
    for artifact in &bundle.artifacts {
        bundle_verified &= store.verify(artifact.content_hash).is_ok();
    }
    let observed_at = now()?;
    let receipt = release_preflight(
        record,
        PreflightEvidence {
            approved: true,
            bundle_verified,
            permission_verified: permission,
            predecessor_matches: predecessor,
            drift_status: drift,
            lease_held: lease,
            staging_proof_valid: staging,
        },
        observed_at.clone(),
    )?;
    ledger.put_receipt(
        record.release_id,
        "preflight",
        &format!("preflight:{}", receipt.receipt_hash),
        &receipt,
        &observed_at,
    )?;
    Ok(receipt)
}

fn read_project_id(root: &Path) -> Result<ProjectId, CoreError> {
    let path = root.join(".neuman/project-id");
    let raw = fs::read_to_string(&path).map_err(|error| {
        CoreError::new(
            "PROJECT_ID_MISSING",
            format!("{}: {error}; run neuman init", path.display()),
        )
    })?;
    ProjectId::from_str(raw.trim())
        .map_err(|error| CoreError::new("PROJECT_ID_INVALID", error.to_string()))
}

fn parse_art_id(value: &str) -> Result<ArtRevisionId, CoreError> {
    ArtRevisionId::from_str(value)
        .map_err(|error| CoreError::new("ART_REVISION_ID_INVALID", error.to_string()))
}
fn parse_release_id(value: &str) -> Result<ReleaseId, CoreError> {
    ReleaseId::from_str(value)
        .map_err(|error| CoreError::new("RELEASE_ID_INVALID", error.to_string()))
}
fn parse_hash(value: &str) -> Result<ContentHash, CoreError> {
    ContentHash::from_str(value)
        .map_err(|error| CoreError::new("CONTENT_HASH_INVALID", error.to_string()))
}

fn report_error(report: core::ValidationReport) -> CoreError {
    CoreError::new(
        "MANIFEST_INVALID",
        report
            .errors
            .into_iter()
            .map(|issue| format!("{} {}: {}", issue.code, issue.path, issue.message))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| CoreError::new("WRITE_DIRECTORY_FAILED", error.to_string()))?;
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, bytes)
        .map_err(|error| CoreError::new("WRITE_FAILED", error.to_string()))?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        CoreError::new("WRITE_COMMIT_FAILED", error.to_string())
    })
}

fn now() -> Result<String, CoreError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| CoreError::new("CLOCK_FORMAT_FAILED", error.to_string()))
}

fn print_human(result: &Value) {
    if let Some(status) = result.get("status").and_then(Value::as_str) {
        println!("{status}");
    } else if let Some(id) = result.get("projectId").and_then(Value::as_str) {
        println!("Initialized project {id}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(result).expect("serializable result")
        );
    }
}

fn exit_code(code: &str) -> i32 {
    if code.contains("INVALID")
        || code.contains("MISMATCH")
        || code.contains("UNKNOWN")
        || code.contains("PREFLIGHT")
    {
        2
    } else if code.contains("AUTH") || code.contains("PERMISSION") || code.contains("KEY") {
        3
    } else if code.contains("CONFLICT") || code.contains("DIRTY") || code.contains("IMMUTABLE") {
        4
    } else if code.contains("UNAVAILABLE") || code.contains("RATE") {
        5
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable_categories() {
        assert_eq!(exit_code("MANIFEST_INVALID"), 2);
        assert_eq!(exit_code("AUTHORIZATION_FAILED"), 3);
        assert_eq!(exit_code("GIT_WORKTREE_DIRTY"), 4);
        assert_eq!(exit_code("PROVIDER_UNAVAILABLE"), 5);
    }

    #[test]
    fn json_envelope_does_not_contain_secrets() {
        let envelope = Envelope {
            schema_version: "1.0",
            ok: false,
            result: Option::<Value>::None,
            error: Some(ErrorBody {
                code: "ROBLOX_OPERATOR_KEY_MISSING",
                message: "environment variable missing".into(),
                retryable: false,
            }),
        };
        let output = serde_json::to_string(&envelope).unwrap();
        assert!(!output.contains(".ROBLOSECURITY"));
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["schemaVersion"],
            "1.0"
        );
    }
}
