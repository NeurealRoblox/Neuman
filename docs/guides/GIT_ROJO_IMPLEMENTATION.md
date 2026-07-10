# Git and Rojo local integration

Status: implemented reference module  
Specification: `/docs/specs/SPEC_11_GIT_AND_ROJO_INTEGRATION.md`
Source: `git_rojo.rs`

## Scope and authority

This module is the local code-lane boundary. Git remains the authority for code and project configuration; Rojo remains the filesystem-to-DataModel compatibility layer. The module does not write Studio state, publish Roblox places, resolve Git conflicts, manufacture credentials, mutate global Git configuration, or treat a dirty developer checkout as a release input.

The intended assembly sequence is:

1. Open the repository top level with `GitClient::open`.
2. Run `probe` and compare the returned Git/Rojo compatibility evidence with project policy.
3. Run `inspect`; show its exact workspace state in the desktop/CLI.
4. Optionally `fetch` a configured remote by name.
5. If the user explicitly requests it, call `update_fast_forward` with the selected upstream ref.
6. Resolve the selected full commit with `validate_exact_commit`.
7. Verify the lockfile's `RojoPin` with `VerifiedRojo::verify`.
8. Create an isolated detached worktree with `create_build_worktree`.
9. Build through a validated `RojoBuildPlan`; ingest and persist its output/receipt in the CAS and build ledger.
10. Only after that persistence succeeds, call `cleanup_build_worktree(receipt, true)`.

Live developer synchronization uses `RojoServePlan` and `RojoSupervisor` against the normal clean developer workspace. A live-sync health result is not build or art-acceptance evidence.

## Public API

### Git discovery and inspection

- `GitClient::open(root)` invokes the system `git` executable. The supplied root must be the exact repository top level; opening an arbitrary nested directory fails.
- `GitClient::open_with_executable` supports a packaged/qualified Git executable and test fixtures without changing command semantics.
- `with_limits` applies per-stream capture and command timeout bounds.
- `probe` records `git --version`, object format, worktree support, optional Git LFS version, and canonical root.
- `inspect` returns exact HEAD, object format, attached/detached branch, upstream, ahead/behind, staged/unstaged/untracked/conflicted counts, merge/rebase/cherry-pick/revert/bisect state, sparse checkout, and partial-clone state.
- `WorkspaceInspection::require_safe_update` rejects dirty, conflicted, detached, or operation-in-progress states. It never cleans, stashes, resets, aborts, or resolves.
- `validate_exact_commit` accepts only a full lowercase 40-hex SHA-1 or 64-hex SHA-256 OID matching the repository format and proves that the object is that exact commit.

`ExactCommit` also validates its invariant during JSON deserialization, so a receipt cannot bypass constructor checks.

### Fetch and fast-forward update

- `fetch(remote, options)` accepts a configured remote **name**, never a remote URL. It captures old and changed remote-tracking refs, timestamp, and bounded diagnostics.
- Pruning is off unless `FetchOptions::prune` is explicitly true.
- Tag behavior is explicit: configured default, all, or none.
- The remote is resolved from Git configuration only for validation. Only credential-free HTTPS, SSH/SCP, and local-file mirror forms are allowed; plain HTTP, custom remote helpers such as `ext::`/`hg::`, URL userinfo, UNC paths, and malformed option-like authorities are rejected.
- Before fetch or update, repository-local/included configuration is inspected and executable or transport-changing settings are rejected, including local credential helpers, SSH commands, filters, HTTP proxy/TLS changes, URL rewrites, protocol overrides, includes, and custom remote upload-pack/proxy helpers. User-owned system/global credential helpers and SSH agents remain available.
- Git receives command-local `protocol.ext.allow=never` and submodule-recursion-off overrides even after validation.
- `update_fast_forward(upstream)` performs a fresh safe-state and local-config check, resolves the target ref to an exact commit, and rejects every target checkout-filter attribute before `git merge --ff-only`. It requires an attached clean branch, never enables autostash, and maps a failed update to conflict or non-fast-forward evidence without inventing a resolution.

All network authentication remains in the user's existing SSH agent, Git Credential Manager, or Git credential helper. The API offers no parameter for a token or password.

### Isolated exact-commit worktrees

- `create_build_worktree` requires a validated exact commit and creates a detached direct child of a dedicated cache root outside the source repository.
- Cache and source roots may not contain one another.
- The target must not already exist and its name is one normalized path component.
- Hooks are disabled for the Git command. Before checkout, all committed `.gitattributes`, local `info/attributes`, and configured global attributes are conservatively inspected. Any checkout `filter` attribute blocks assembly. This prevents an untrusted repository from causing a configured clean/smudge/process filter to execute during checkout.
- The created worktree is re-inspected for exact HEAD, clean state, detached state, and absence of an in-progress Git operation.
- A sibling `NAME.neuman-worktree.json` receipt is written atomically outside the worktree. It is written before final validation so a partially qualified/orphaned creation is still discoverable.
- `cleanup_build_worktree` requires the durable receipt, `artifact_persisted = true`, the same repository/cache/direct-child path, the same exact commit, a clean detached worktree, and no active operation.
- Cleanup uses ordinary `git worktree remove`, not `--force`. It therefore cannot silently delete unexpected files. Git metadata is pruned only after successful removal, and a separate cleanup receipt is persisted.
- `reconcile_build_cache` is read-only. It compares persisted receipts, filesystem paths, and `git worktree list --porcelain`, returning inconsistencies for operator review. It never automatically removes an orphan.

### Pinned Rojo verification and build

- `RojoPin` contains an explicit executable path, exact version, and lowercase SHA-256 from a trusted lockfile.
- `VerifiedRojo::verify` canonicalizes the file, requires a regular file, streams its SHA-256, invokes that exact path with `--version`, and accepts only `Rojo VERSION` with an exact match.
- `VerifiedRojo` is a non-deserializable capability type with private fields. External code cannot manufacture verified state; it must call `verify`.
- `RojoBuildPlan::create` validates a normalized relative project path inside the exact worktree and a normalized new `.rbxl` or `.rbxlx` output beneath a dedicated output root. Input and output roots may not contain one another. Existing outputs are rejected.
- The project file SHA-256 is rechecked immediately before `rojo build`. The output must be a non-empty regular file, not a symlink. The receipt records output path, size, SHA-256, project SHA-256, Rojo version/checksum, timestamp, and bounded diagnostics.
- The output path is only staging evidence. The caller must ingest it into NeuMan CAS and persist the build/bundle receipt before worktree cleanup or publication.

### Rojo live-server supervision

- `RojoServePlan::create` accepts only a project inside the workspace, an exact verified Rojo capability, a nonzero port, bounded log size, and bounded explicit restart count.
- The server binds only `127.0.0.1` using `rojo serve PROJECT --address 127.0.0.1 --port PORT`.
- `RojoSupervisor` owns exactly one child. `snapshot` observes PID, starting/healthy/exited/stopped state, loopback TCP reachability, restart attempts, exit code, and redacted bounded logs.
- `restart` is explicit, only allowed after exit/stop, and refuses to exceed the restart budget.
- `stop` handles both already-exited and live children, killing only its owned child and waiting for it. `Drop` is a final best-effort orphan guard.
- A reachable port is only process-health evidence. Studio/plugin association, Rojo protocol compatibility, connected-client identity, last applied patch, ownership preview, and sync freshness remain daemon/plugin responsibilities.

### Desktop live-session manager

`RojoSessionManager` is the desktop/daemon lifecycle boundary above `RojoSupervisor`. It is designed to live behind a Rust-side mutex and exposes only serializable request/status objects to the UI.

- `RojoSessionKey::create` canonicalizes a workspace and validates a single-component manifest place key. The manager's equality/order identity is a normalized canonical workspace identity plus place key, including case normalization on Windows. This prevents two differently-cased spellings of the same Windows workspace from receiving separate children.
- Exactly one retained record and at most one owned child may exist for a workspace/place key. `start` is idempotent when the requested context is identical: it returns `created: false` and the existing PID/port/status rather than spawning another process.
- A retained exited/stopped record is also returned by idempotent `start`; the caller must use the explicit `restart` action or remove the inactive record before creating a fresh lifecycle. This prevents a generic UI retry from silently consuming restart budget or hiding a crash.
- Every `start` re-runs `VerifiedRojo::verify` against the requested lockfile path/version/SHA-256 before it compares or creates context. A caller cannot bypass verification by deserializing `VerifiedRojo`.
- A context ID is SHA-256 over domain-separated, length-delimited canonical workspace, place key, canonical project path, project-file SHA-256, Rojo version, and Rojo executable SHA-256. Changing the project bytes/path or pin while a record exists yields `GIT_ROJO_SESSION_CONFLICT`; replacement requires an explicit stop and inactive-record removal.
- A session ID is a stable truncated SHA-256 identity of canonical workspace/place. It remains stable across controlled restart and context-preserving recreation.
- Port selection starts at a SHA-256-derived offset for workspace/place and probes the configured inclusive range in deterministic wraparound order. It excludes all ports reserved by retained manager records and tests each candidate with a temporary loopback bind. The default policy range is `34872..=34971`; deployments may supply a validated non-privileged range of at most 16,384 ports.
- The temporary availability probe cannot atomically hand the socket to Rojo. A process racing to claim the selected port will cause Rojo to exit, which is reported as an unexpected child exit; the manager never searches for or kills the competing process.
- `status_by_key`, `status`, and `list` poll child state and return session/context identity, exact project/Rojo hashes, PID evidence, deterministic port, start/observation timestamps, restart count, bounded redacted logs, and the retained last-exit report.
- Unexpected process exit is classified separately from a requested stop or requested restart. The report retains platform exit code when available and observation time across a later restart.
- `stop` terminates and waits only through the `Child` handle stored in that session's `RojoSupervisor`. PID is display evidence and is never accepted as an operation input.
- `restart` performs a controlled stop when needed and starts through the existing verified plan, enforcing project-file hash freshness and the configured restart budget. It preserves session/context/port identity.
- `stop_all` attempts every owned session even if one cleanup fails. `remove_inactive` only removes an exited/stopped record after proving no child handle remains. Dropping the manager best-effort stops every still-owned child; the underlying supervisor is an additional orphan guard.

Recommended desktop commands are `rojo_session_start(request)`, `rojo_session_status(workspace, placeKey)`, `rojo_session_list()`, `rojo_session_stop(workspace, placeKey)`, `rojo_session_restart(workspace, placeKey)`, and `rojo_session_remove_inactive(workspace, placeKey)`. Each command should lock one shared `RojoSessionManager`, construct the key inside Rust, and return `RojoSessionStatus`; the webview must never supply a PID or executable capability.

## Process and credential safety

Every Git/Rojo child is constructed with `std::process::Command` and an argument vector. No shell is invoked. Git receives command-local `core.hooksPath`, `core.fsmonitor=false`, `protocol.ext.allow=never`, and no-submodule-recursion overrides; no repository, system, or global configuration is written.

The child environment is cleared and rebuilt from a narrow interoperability allowlist: executable/system paths, home/profile paths needed to read normal Git configuration, temporary/application paths used by platform helpers, locale, SSH agent/askpass endpoints, and Git Credential Manager interaction mode. `GIT_TERMINAL_PROMPT=0` prevents an invisible password prompt. Credential values are never accepted by the public API, placed in arguments, or written into receipts.

Each one-shot command:

- has a timeout;
- continuously drains stdout and stderr to avoid pipe deadlock;
- retains at most the configured byte count per stream;
- marks truncation and timeout explicitly;
- retains a stable operation label but not its argument vector;
- redacts HTTP URL userinfo and authorization-header lines before returning diagnostics.

Paths passed to external tools normalize Windows verbatim canonical prefixes so system Git/Rojo receive native usable paths while all ownership comparisons continue to use canonical paths.

## Failure behavior and codes

The module emits the normative SPEC-11 codes where applicable:

- `GIT_REPOSITORY_UNTRUSTED`
- `GIT_WORKTREE_DIRTY`
- `GIT_OPERATION_IN_PROGRESS`
- `GIT_UPDATE_NOT_FAST_FORWARD`
- `GIT_CONFLICT`
- `GIT_WORKTREE_CREATE_FAILED`
- `GIT_ROJO_VERSION_MISMATCH`
- `GIT_ROJO_SERVER_FAILED`
- `GIT_SOURCEMAP_STALE`
- `GIT_ROJO_SESSION_CONFLICT`
- `GIT_ROJO_PORT_UNAVAILABLE`

Additional narrow local codes identify malformed input, unsafe paths, invalid receipts, process timeout/start/pipe failures, unsupported object formats, unavailable repositories, fetch/status failures, detached/unborn HEAD, and blocked cleanup. Errors contain redacted bounded evidence and preserve user state.

## Verification

The module has fifteen tests:

1. URL-userinfo and authorization-header redaction.
2. Exact SHA-1/SHA-256 OID validation, including deserialization rejection.
3. Normalized relative path enforcement.
4. Exact Rojo version-output parsing.
5. Remote-transport allowlisting while retaining safe HTTPS/SSH/local-mirror syntax.
6. Conservative checkout-filter detection.
7. Dangerous repository-local Git configuration classification.
8. Real fetch rejection before a repository-local executable credential helper can run.
9. Real target checkout-filter rejection before a fast-forward mutates the worktree.
10. Real temporary-repository clean/dirty inspection and fail-closed update precondition.
11. Real exact detached-worktree creation, persisted receipt, blocked early cleanup, and clean non-force cleanup.
12. Real local bare-remote fetch followed by an explicit fast-forward-only update.
13. Portable compiled fake-Rojo lifecycle: checksum/version verification, healthy start, idempotent duplicate start, context-drift rejection, requested stop, controlled restart, deterministic identity/port recreation, inactive removal, and stop-all.
14. Portable compiled fake-Rojo crash with exit code 17 and unexpected-exit/log reporting.
15. Fail-closed live-session port-range, workspace, and place-key validation.

Verification performed on Windows against the standalone module harness:

```text
cargo test git_rojo::tests::: 15 passed; 0 failed
cargo clippy --all-targets -- -W clippy::pedantic -D warnings: passed
rustfmt --edition 2024 --check: passed
```

The tests use temporary repositories and only repository-local Git identity configuration. Live-session tests compile a tiny cross-platform fake Rojo executable with the active Rust toolchain, pin its actual SHA-256, verify its exact `Rojo 7.7.1` response, and exercise real child processes and loopback listeners. They do not mutate global Git configuration or use network credentials.

## Wiring into the current flat crate

The root integration is intentionally small:

```rust
// neuman_lib.rs
pub mod git_rojo;
```

CLI/desktop commands should hold `GitClient` and one shared `RojoSessionManager` on the Rust side. Build commands may temporarily hold `VerifiedRojo`; live child capabilities remain encapsulated by the manager. The webview receives serialized observations/receipts only; it must never receive process, filesystem, credential-helper, PID-kill, or child-control capabilities.

The desktop manifest adapter resolves:

- `repository.projectFile` to the relative project path;
- repository object format to the probe result;
- lockfile Rojo executable/version/SHA-256 to `RojoPin`;
- project policy to fetch tag/prune behavior and the allowed build cache/output roots;

Before a desktop start or controlled restart, `rojo_desktop_config` also parses the strict Rojo project graph, recursively resolves `$path`/project includes without symlinks or workspace escape, derives exact/subtree DataModel claims, and reconciles every claim and source against the selected place's `git-code` ownership and `projectPath`. Unknown/dynamic constructs fail closed. Binary model inputs require an explicit `allowRojoBinaryModels: true` owner option and can never cross a delegated or Studio-owned root. The Rust-only report records project files and path-by-path authorization evidence.
- the selected build commit to a full OID before worktree creation.

The CLI should expose inspect/fetch/ff-update as separate commands. A build command should persist the `RojoBuildReceipt` plus NeuMan CAS/build receipt before setting `artifact_persisted = true`. Live serve should use the retained manager rather than directly exposing or spawning a detached unmanaged supervisor.

## Deliberate gaps requiring separate qualification

These are not silently claimed complete:

- Git version-range comparison, safe-directory ownership remediation, case/long-path probes, commit-signing policy, and credential-helper availability need the project compatibility-policy layer. This module reports the raw starting evidence and never changes those settings.
- Git rebase/merge-local-commits/manual update strategies are not implemented. Fast-forward-only is the only mutation path.
- Submodule initialization is not performed. Submodules remain uninitialized unless a future explicit, trust-reviewed policy adds them.
- Git LFS hydration is intentionally blocked by the conservative checkout-filter rule. A qualified LFS provider step must validate `.lfsconfig`/endpoint, pin or qualify Git LFS, fetch required objects, verify pointer SHA-256/size, and then materialize without weakening the untrusted-filter boundary.
- `servePlaceIds` policy matching, destructive-patch preview, plugin/server major-version handshake, source-map generation, and Studio patch evidence still require the daemon/plugin compatibility layer and the P0-08 Studio corpus. Ownership parsing and include/path reconciliation are implemented as a strict fail-closed desktop subset; broadening it to JSONC, sync rules, or other Rojo features requires version-pinned corpus qualification.
- Rojo verification and live serve are exercised end-to-end with a checksum-pinned fake executable, including health, crash, stop, restart, and deterministic recreation. Real Rojo build/serve still requires Windows/macOS/Linux qualification against the supported pinned Rojo matrix and minimal/representative projects, including port races, output disappearance, log flood, plugin mismatch, and Studio upgrade cases.
- Native art-cell insertion and reference validation happen after Rojo assembly in the Studio runner; this module only produces the code/declarative candidate.

Until those qualifications pass, the safe supported surface is exact Git inspection/fetch/ff-only update, filter-free isolated worktree assembly primitives, pinned Rojo identity enforcement, and the managed loopback live-session lifecycle. Production publication must still require the broader build and release preflight evidence.
