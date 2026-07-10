# SPEC-11 — Git and Rojo Integration

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-03, SPEC-09

## 1. Purpose

This specification defines Git repository behavior, branch/worktree synchronization, dirty/conflict handling, Git safety, Rojo project compatibility, server supervision, ownership partitions, source maps, build/syncback behavior, and the boundary between Git-owned code and Studio-owned art.

## 2. Authority rule

Git is the editing and history authority for `git-code` roots and committed project configuration. Studio may display and execute synchronized code, but a Studio state with no corresponding filesystem/Git state is not a release input.

Rojo is the mapping/transport compatibility layer between the filesystem and Git-owned DataModel roots. NeuMan MUST NOT invent a second code-path mapping format.

## 3. Git implementation strategy

Reference implementation uses the system Git executable for network, checkout, merge/rebase, worktree, LFS, signing, and credential-helper compatibility. Read-only status/object inspection MAY use a Rust library if results are cross-checked against Git behavior.

Reasons:

- SSH agent and Git Credential Manager compatibility;
- partial clone/worktree/LFS behavior;
- signed commits and evolving object formats;
- familiar user diagnostics.

Commands execute as argument arrays without a shell. Environment is allowlisted. Credentials never enter args/URLs/logs.

## 4. Git version and features

Project toolchain records tested Git range. Startup probes:

- resolve the current-platform Rojo artifact only from `neuman.lock.json` plus a native-selected tool root;
- require the lockfile to bind the validated project manifest hash;
- reject absolute/traversing/artifact paths or canonical paths escaping the tool root;
- verify the executable SHA-256 and exact reported Rojo version before every new build/session;
- never accept an executable path, checksum, port, or PID from the webview.

- `git --version`;
- object format;
- worktree support;
- LFS installation/version when provider requires it;
- credential helper availability;
- long paths/case sensitivity behavior;
- safe-directory status without modifying global config silently;
- signing configuration status where policy requires.

Unsupported version is compatibility error before mutation.

## 5. Repository identity and trust

Identity includes GitHub repository numeric ID when available, normalized remote, object format, and initial root. Repository path ownership warnings are resolved explicitly; NeuMan MUST NOT globally add arbitrary safe directories without user approval.

Trust review includes:

- remotes and URLs;
- submodules;
- hooks;
- `.lfsconfig` and LFS endpoint;
- project/lock manifests;
- production targets;
- executable filters/diff drivers;
- extensions/build commands.

Security-expanding changes trigger renewed trust.

## 6. Workspace states

Git summary:

- HEAD and branch/detached state;
- upstream and ahead/behind;
- staged, unstaged, untracked, ignored, conflicted;
- merge/rebase/cherry-pick/bisect operation state;
- sparse checkout/partial clone/submodules/LFS health;
- index/worktree filesystem errors.

NeuMan MUST NOT auto-clean or auto-abort an existing Git operation.

## 7. Fetch

- `fetch` is non-destructive to worktree.
- Default fetches configured remote and prunes only when user/project policy enables it.
- Tags follow project policy.
- Authentication uses existing Git mechanisms.
- Provider/network errors preserve state and include retry guidance.
- Fetch result records old/new remote refs and time.

## 8. Update strategies

Project/user chooses one:

- fast-forward only (default);
- rebase local commits;
- merge upstream;
- manual.

Rules:

- dirty worktree blocks update unless user explicitly stashes/commits or selected Git operation safely supports it;
- NeuMan never invents conflict resolutions;
- autostash is off by default and, if used, is named/recorded and never silently dropped;
- branch protection is respected;
- detached HEAD is read-only for normal update until user creates/selects branch;
- successful update triggers Rojo reconciliation only after Git operation fully completes.

## 9. Worktrees

NeuMan SHOULD use Git worktrees for isolated clean builds and MAY use them for multiple developer workspaces.

- Build worktree uses exact commit, detached, read-only input policy.
- Created under daemon cache, not repository source root.
- LFS objects materialized according to build needs.
- No user untracked files copied.
- Cleanup occurs only after artifact references persisted.
- Orphaned worktrees reconciled on startup through Git worktree metadata.

## 10. Commit creation

When enabled, NeuMan can create commits for machine-authored art pointers, lockfiles, manifests, and release receipts.

- Shows exact staged paths/diff.
- Does not stage unrelated user changes.
- Uses explicit pathspec from generated files.
- Commit message includes human summary and machine trailers without secrets.
- Author is user for user action; bot/App only when acting as automation.
- Signing follows repository policy.
- Failed commit leaves staging state visible and recoverable.

NeuMan does not automatically commit developer code unless explicitly requested.

## 11. Branches and PR preparation

Generated branch prefix default `neuman/`, configurable. Branch names sanitize user input and avoid collision. Creating art adoption/proposal branches records base commit and art channel head.

Force push is never default. Protected branch updates occur through PR/check workflows.

## 12. Git conflicts

Text conflicts remain standard Git conflicts. UI links files and provides external-editor action. NeuMan does not parse conflict markers as valid source.

Binary/LFS art pointer conflicts are resolved using ArtRevision/cell semantics, not `git checkout --ours` without context. After art conflict resolution, NeuMan writes canonical pointer/manifest and stages only resolved files.

## 13. Git LFS integration boundary

Detailed storage behavior is SPEC-17. Git/Rojo integration must ensure:

- required LFS objects present before build/apply;
- pointer file never mistaken for native `.rbxm`;
- LFS fetch/checksum failures block build;
- `.gitattributes` changes are trust-relevant;
- LFS locks are informational only unless provider is proven/enforced; Hub cell locks remain authoritative.

## 14. Rojo versioning

Rojo version is constrained by manifest and exactly pinned/checksummed in lockfile. Supported source:

- bundled signed binary;
- project tool manager such as Rokit, after checksum/version verification;
- system binary only when exact policy permits.

NeuMan never silently upgrades Rojo. Plugin major version must match server as required by Rojo.

## 15. Rojo project validation

NeuMan parses or invokes Rojo to validate `default.project.json/jsonc` and included projects. It resolves:

- mapped filesystem paths;
- DataModel target paths;
- `servePlaceIds`, `placeId`, `gameId`;
- unknown-instance behavior;
- syncback rules;
- binary/XML model inputs;
- included project files;
- output type and project root.

Resolved Rojo targets are compared with NeuMan ownership. Any overlap with `studio-art`, `terrain`, or `service-state` that permits Rojo deletion/replacement is a blocking configuration error.

The desktop preflight supports strict `.project.json` plus the documented v7 instance directives `$className`, `$path`, `$properties`, `$attributes`, and `$ignoreUnknownInstances`. It recursively resolves explicit project includes and `default.project.json` directory includes, with bounded project/node/source counts. JSONC projects, unknown project fields/directives, custom sync rules, non-empty glob ignore rules, project-selected serve addresses, missing inputs, path traversal, and symlinked source components/entries are blocking until a version-pinned parser models their exact semantics. This is an intentional fail-closed compatibility subset, not a claim that Rojo itself lacks those features.

For reconciliation, a filesystem mapping controls its DataModel destination subtree even when unknown instances are preserved, because future Git changes can introduce any descendant. A node whose effective `ignoreUnknownInstances` is true also controls the subtree; properties/attributes and non-pass-through declarations control their exact destination. Each claim must be wholly contained by one `git-code` ownership root and may not cross any delegated child root. Each `$path` must remain below that owner's declared `projectPath`. `.rbxm` and `.rbxmx` content is ambiguous to a structural preflight and requires `allowRojoBinaryModels: true` on the containing `git-code` owner; that override cannot authorize overlap with Studio-owned state.

## 16. Ownership-safe Rojo configuration

Required principles:

- Git-owned roots are explicitly mapped.
- Studio-art roots are outside Rojo management or protected through `ignoreUnknownInstances`/equivalent behavior validated for current version.
- Generated roots have only one generator.
- `servePlaceIds` SHOULD list exact permitted developer/authoring sandbox places.
- A Rojo initial sync preview is mandatory on a new place/session.
- Large deletion/replacement patches require confirmation per project policy.
- Desktop start and controlled restart rerun the ownership preflight before spawning Rojo; status and stop remain read/control operations and do not reinterpret project configuration.

NeuMan maintains an ownership compatibility report with path-by-path explanation.

## 17. Rojo server supervision

Start inputs:

- exact executable/checksum;
- exact project path/hash;
- available loopback port;
- workspace root;
- sanitized environment;
- log/output limit.

Tracked state:

- PID/process start identity;
- server version;
- port and health endpoint;
- project/config hash;
- connected client/session count if observable;
- last patch/error;
- restart attempts.

Changing project manifest triggers controlled restart only after user-visible reconciliation if it changes ownership or target.

## 18. Rojo live sync

Flow:

1. Git/filesystem reaches stable state.
2. Rojo detects changes.
3. Plugin/Studio previews/applies per Rojo configuration.
4. NeuMan observes server health and Studio session association.
5. Code lane reports Git HEAD and Rojo sync freshness separately.

Rojo success does not imply build success. A connected Studio may still have art conflicts or wrong accepted revision.

## 19. Rojo two-way/syncback

Default policy:

- Git code remains filesystem-authored; Studio-to-filesystem code sync is off unless project opts in.
- Art baseline/capture uses NeuMan native cells, not generic Rojo syncback, for supported fidelity path.

If code two-way is enabled:

- changes write only within Git-owned routes;
- resulting files are dirty worktree changes and require normal review/commit;
- conflict with simultaneous filesystem edit stops and preserves both;
- script/property types unsupported by Rojo remain blocked/diagnosed;
- never advance Git branch automatically.

Rojo `syncback` MAY assist initial project migration, but output receives full review, native fidelity comparison, and ownership mapping before acceptance.

## 20. Source maps

NeuMan generates and records Rojo source-map hash for:

- editor/LSP integration;
- mapping Studio Script to repository file;
- build diagnostics;
- code ownership validation.

Source map is derived, usually ignored by Git, and regenerated from exact project/commit. Stale source map is warning/block based on action.

## 21. Rojo build

Build command runs in clean worktree with exact output path and binary/XML choice. Native art strategy:

- Rojo builds code, declarative instances, and art slots/place base;
- native Studio art cells are inserted/validated by Studio runner for full-fidelity path;
- external Open Cloud artifact path may include supported `.rbxm` through Rojo only after compatibility validation.

Output is never published directly from a developer dirty worktree.

## 22. Failure handling

- Rojo process crash: preserve logs, mark sync stale, bounded restart.
- Plugin mismatch: stop server/apply and show install action.
- Patch failure: identify failed instance/property; do not claim synchronized.
- Massive replacement: require review and ensure art roots excluded.
- Reference fallback replacement risk: validator checks cross-instance references after sync.
- Source file disappears mid-build: fail cleanly, no partial artifact promotion.

## 23. Error codes

- `GIT_VERSION_UNSUPPORTED`
- `GIT_REPOSITORY_UNTRUSTED`
- `GIT_WORKTREE_DIRTY`
- `GIT_OPERATION_IN_PROGRESS`
- `GIT_UPDATE_NOT_FAST_FORWARD`
- `GIT_CONFLICT`
- `GIT_LFS_OBJECT_MISSING`
- `GIT_CREDENTIAL_FAILED`
- `GIT_WORKTREE_CREATE_FAILED`
- `GIT_ROJO_VERSION_MISMATCH`
- `GIT_ROJO_OWNERSHIP_OVERLAP`
- `GIT_ROJO_SERVER_FAILED`
- `GIT_ROJO_PATCH_FAILED`
- `GIT_ROJO_SYNC_STALE`
- `GIT_SOURCEMAP_STALE`

## 24. Acceptance criteria

1. Dirty/conflicted/in-progress Git states are never overwritten.
2. Credentials never enter process args, remote URL, or logs.
3. Ownership corpus proves Rojo cannot delete Studio-art roots.
4. Rojo version/plugin mismatch blocks mutation.
5. Clean build worktree contains exact commit and required LFS objects only.
6. Two-way code sync produces visible Git changes and preserves concurrent conflicts.
7. Rojo crash/restart does not duplicate or misroute sessions.
8. Source-map mapping is reproducible from pinned commit/project.

## 25. References

External sources last verified: 2026-07-09.

- [Rojo repository](https://github.com/rojo-rbx/rojo)
- [Rojo project format](https://rojo.space/docs/v7/project-format/)
- [Rojo sync details](https://rojo.space/docs/v7/sync-details/)
