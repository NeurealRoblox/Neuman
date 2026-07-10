# SPEC-05 — Desktop Application

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-01–04

## 1. Purpose and technology

The desktop application is the primary human control surface. The reference implementation SHOULD use Tauri 2, a Rust command boundary, and React/TypeScript UI. The webview cannot directly access filesystem, process, credential, Git, Studio, or network mutation capabilities; all privileged actions cross typed Rust commands.

The desktop SHALL be a client of the local daemon even when both ship in one bundle. UI reload/crash MUST NOT terminate builds, transfers, locks, or releases owned by the daemon.

## 2. Product principles

- Outcome and current truth appear before implementation detail.
- Code, art, build, and deployment state are visually distinct.
- Destructive and production-impact actions are never ambiguous.
- Every blocked action explains the failed gate and remediation.
- The UI never claims stronger certainty than evidence supports.
- Offline/read-only state is useful and explicit.
- Progressive disclosure keeps common workflows simple without hiding expert details.

## 3. Application lifecycle

1. Verify signed application bundle and compatible daemon.
2. Acquire single desktop activation; subsequent launches focus the existing window.
3. Connect to daemon over authenticated local IPC.
4. Load non-secret preferences and account summaries.
5. Restore windows only after project paths are revalidated.
6. Show recovery banner for interrupted operations.
7. On close, offer hide-to-background when active operations/locks exist; never silently abandon them.
8. On update, wait for a safe point or explicitly transfer operation ownership to the compatible daemon.

## 4. Global navigation

Primary destinations:

1. Portfolio
2. Workspace
3. Art Review
4. Builds
5. Releases
6. Activity
7. Diagnostics
8. Settings

Context header always shows active project, workspace/branch, selected place, optional self-hosted Hub connectivity, Studio connection, and whether the current context has production impact.

## 5. Setup wizard

Steps:

1. Welcome and deployment mode: local or connect to a user-operated/self-hosted Hub; no NeuMan account or official hosted endpoint exists.
2. Roblox sign-in using system browser and PKCE.
3. GitHub sign-in/App installation or generic Git configuration.
4. Select/clone existing repository or initialize only after explicit request.
5. Discover or create project manifest.
6. Select Roblox authoring/staging/production targets using OAuth-visible resources.
7. Detect/install compatible Rojo and Studio plugin.
8. Ownership mapping wizard with visual DataModel paths.
9. Art baseline capture or existing art revision selection.
10. Validation summary; unresolved critical items block completion.

The wizard supports save-and-resume. It never writes production configuration until the review step confirms exact universe/place IDs.

## 6. Portfolio screen

For every project:

- project name and repository;
- auth health;
- Hub health;
- authoring/staging/production target summaries;
- latest accepted art revision;
- latest code head and CI status;
- latest staged/production release;
- drift status and confidence;
- blocking alerts.

Portfolio data is timestamped. Stale data displays age and refresh state.

## 7. Workspace screen

The central view has three lanes.

### 7.1 Code lane

- repository, branch, HEAD, upstream divergence;
- clean/dirty/untracked/conflicted counts;
- last fetch and CI state;
- Rojo process/connection/source-map status;
- safe actions: fetch, update, resolve, open editor, build;
- no one-click destructive discard without a file preview and confirmation.

### 7.2 Art lane

- selected channel and accepted revision;
- local applied revision/state hash;
- incoming/outgoing/dirty/conflicted cells;
- lock ownership and lease health;
- plugin and Studio target identity;
- actions: checkpoint, propose, compare, accept when authorized, apply, stash draft.

### 7.3 Deployment lane

- logical build identity;
- staging and production deployment markers;
- drift evidence/confidence;
- release gates;
- actions: create build, compose release, inspect rollback.

Lane-to-lane arrows show the exact code/art inputs feeding a build and the exact build feeding a deployment.

## 8. Studio connection panel

For each Studio session:

- Studio version/channel;
- plugin version/protocol;
- Studio user ID/username;
- universe/place IDs and names;
- mode: edit/play/test/run;
- active project/place/art channel;
- paired daemon installation;
- latency, last heartbeat, queued changes;
- compatibility and identity warnings.

Wrong-place detection blocks mutations and offers navigation/instructions rather than an override by default.

## 9. Art review screen

### 9.1 Revision header

- revision ID/state root;
- parents;
- author/session/place;
- message/time;
- validation and approval state;
- Studio/schema version;
- changed cell/terrain/service counts and total bytes.

### 9.2 Cell list

Filter/sort by name, kind, owner, size, validation, lock, dependency, path, and change type. Each row shows old/new hash, semantic summary, preview, and external-edge count.

### 9.3 Diff viewer

Views:

- hierarchy additions/removals/moves;
- property changes with typed values;
- transform before/after;
- asset/package dependency changes;
- external references;
- validation findings;
- side-by-side preview where available;
- raw metadata and native hash for expert audit.

The UI labels semantic indexes as review representations and never claims they are complete reconstruction data.

### 9.4 Conflict resolver

Options are ours, theirs, duplicate-as-new-cell, open comparison place, or cancel. It MUST show base/ours/theirs identities. No default resolution is preselected for a binary conflict.

## 10. Build screen

- input manifest with code/art/dependencies/toolchain/policy;
- each build stage and duration;
- live structured logs with severity/component filtering;
- validation findings with source links;
- artifact hashes/sizes;
- cancellation when safe;
- retry behavior clearly stating whether same BuildId/attempt or new logical build;
- export signed build receipt.

Successful build prominently distinguishes logical build hash from raw artifact hashes.

## 11. Release composer

Steps:

1. Select a successful build/release bundle.
2. Select environment and place set.
3. Verify target IDs/names/creator and publication method.
4. Review drift and current deployment.
5. Review ordered rollout and rollback target per place.
6. Review gates and staged proof.
7. Enter release notes.
8. Request/collect approvals.
9. Final confirmation with typed production phrase if policy requires.
10. Observe per-place progress and partial failure state.

Changing bundle, target, order, method, or policy after approval invalidates affected approvals.

## 12. Release detail

- immutable request and approval evidence;
- per-place preflight, publish, verification, restart, rollback steps;
- provider request/response receipts redacted;
- current and previous Roblox version numbers;
- timestamps and actor;
- retry and rollback actions gated by current state.

Partial success uses a dedicated high-severity screen, not a generic error toast.

## 13. Drift workflow

States display confidence. For drift:

- inspect evidence;
- recapture through Studio when needed;
- compare with expected build;
- create an adoption proposal;
- explicitly discard unauthorized external change only through a new release of accepted inputs;
- waive a gate only with permission, reason, expiry/scope, and audit.

## 14. Activity and audit

Timeline filters by project, actor, code, art, build, release, lock, auth, and failure. Audit entries show correlation IDs and immutable object links. Sensitive raw payloads are not rendered.

## 15. Diagnostics

- application/daemon/plugin/Rojo/Studio/Hub versions;
- compatibility matrix result;
- file locations and disk usage;
- connectivity tests;
- keychain record health without secret values;
- API scope/resource summary;
- redacted logs and support-bundle preview;
- self-check actions that do not mutate projects by default.

## 16. Settings

Categories:

- Accounts
- Git/GitHub
- Roblox
- Studio/plugin
- Hub
- Storage/cache
- Notifications
- Accessibility
- Privacy/telemetry
- Updates
- Advanced/experimental

Experimental Lore or unsafe compatibility overrides are clearly labeled, project-scoped, and auditable.

## 17. Offline behavior

The UI displays one of:

- online;
- degraded external provider;
- Hub offline/local available;
- fully offline;
- auth expired.

Allowed offline: read cached history, edit Git files externally, local draft capture if configured, local validation/build steps not needing network. Disallowed: shared lock acquisition, accepted protected-channel update, centralized approval, publication, authoritative drift clean result.

Queued operations show exact prerequisites and require revalidation before mutation after reconnect.

## 18. Notifications

In-app notifications are categorized:

- informational;
- action required;
- conflict;
- security;
- production impact.

OS notifications contain no private project content by default. Production and security notifications cannot be permanently disabled, though sound may be.

## 19. Error UX

Every error surface includes:

- plain-language outcome;
- stable error code;
- affected object/target;
- whether anything changed;
- retryability;
- remediation buttons;
- correlation ID and diagnostic link.

Toasts are used only for non-critical transient outcomes. Conflicts, auth failures, corruption, and partial publication require persistent surfaces.

## 20. Accessibility

- WCAG 2.2 AA target.
- Full keyboard operation and visible focus.
- Screen-reader names/state for all interactive controls.
- Color is never the only state signal.
- Reduced motion honored.
- Text zoom to 200% without loss of function.
- High-contrast theme.
- Tables provide accessible summaries and navigation.
- Large diffs use virtualization without breaking assistive technology reading order.

## 21. Performance budgets

On baseline hardware after warm start:

- first interactive shell: target <2 seconds;
- project switch cached summary: <500 ms;
- UI response to input: <100 ms p95;
- 10,000-cell list scroll at 60 fps target;
- logs/diffs stream incrementally; no full in-memory render requirement;
- background CPU idle <1% target excluding active watchers.

Budgets are measured in SPEC-20; failure is visible in release gates.

## 22. Security boundary

- UI receives opaque credential status only.
- URLs opened from project content require allowlist/confirmation.
- HTML/Markdown from providers is sanitized.
- Tauri command allowlist is explicit; no generic shell command.
- File pickers return handles validated by core.
- Clipboard use for IDs/codes is explicit and clears sensitive pairing codes after timeout where supported.

## 23. Acceptance criteria

1. End-to-end usability tests cover setup, sync, conflict, build, release, partial failure, rollback, and drift.
2. All high-impact actions display exact target and immutable input.
3. UI remains accurate after daemon restart and operation reconnect.
4. Accessibility audit meets WCAG target.
5. No privileged operation can be invoked with parameters not revalidated by Rust/core.
6. Offline mode never implies authoritative team state.
7. Performance budgets pass on documented baseline hardware.
