# SPEC-06 — Core Daemon and CLI

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02–04

## 1. Responsibilities

The Rust core/daemon is the local authority for:

- manifest parsing and policy evaluation;
- local project/workspace registry;
- credential-vault access;
- Git/Rojo/Studio process supervision;
- local CAS and metadata database;
- plugin pairing and loopback protocol;
- art transfer, validation, indexing, and apply orchestration;
- build and release operations;
- provider clients;
- structured logs, metrics, audit staging, and support bundles;
- typed command API consumed by desktop and CLI.

The daemon MUST NOT trust the desktop renderer, plugin, repository, Hub, or provider response without validation.

## 2. Process model

- One daemon installation instance per OS user and release channel.
- A platform single-instance lock prevents duplicate ownership of discovery port, local DB, and operations.
- Desktop and CLI connect over authenticated local IPC.
- Daemon MAY run in foreground for debugging or as a user background process.
- Crashes are restartable; durable operation state is recovered from the local database.
- Version upgrades use an explicit data migration and compatibility handshake.

## 3. Local directories

Windows:

```text
%LOCALAPPDATA%/NeuMan/<channel>/
  config/
  data/neuman.db
  cache/cas/
  cache/builds/
  logs/
  run/
  updates/
```

macOS:

```text
~/Library/Application Support/NeuMan/<channel>/...
~/Library/Caches/NeuMan/<channel>/...
~/Library/Logs/NeuMan/<channel>/...
```

The keychain stores secrets. Project-local `.neuman` data follows SPEC-03. Paths are returned through diagnostics and never guessed by UI.

## 4. Local database

Reference: SQLite in WAL mode with foreign keys enabled.

Stores:

- projects/workspaces;
- external account non-secret metadata and keychain references;
- operations/attempts;
- local art drafts/revision cache;
- artifact references/refcounts;
- Studio/plugin sessions;
- provider cursors/ETags;
- queued Hub events;
- audit staging;
- update/migration records.

Schema migrations are transactional, numbered, forward-only in normal operation, and backed up before destructive changes. Database corruption enters read-only recovery; it MUST NOT auto-delete and recreate.

## 5. Local IPC

Use OS-local IPC with peer-user validation:

- Windows named pipe with user-specific ACL;
- macOS Unix domain socket mode `0600` in user runtime directory.

Protocol is versioned request/response plus server events. Each client authenticates with an installation-local random IPC secret held by the launcher/CLI through protected storage or inherited handle. TCP localhost is not the default desktop IPC.

Commands have:

- command name/version;
- request ID/idempotency key;
- expected project/workspace version where applicable;
- typed payload;
- cancellation token for long operations.

## 6. Module boundaries

Reference Rust crates/modules:

- `neuman-domain`
- `neuman-config`
- `neuman-policy`
- `neuman-crypto`
- `neuman-cas`
- `neuman-art`
- `neuman-git`
- `neuman-rojo`
- `neuman-roblox`
- `neuman-github`
- `neuman-hub-client`
- `neuman-build`
- `neuman-release`
- `neuman-observability`
- `neuman-daemon`
- `neuman-cli`

Provider traits are dependency-inverted; domain code does not depend on UI or a specific hosted service.

## 7. Project registration

`project add`:

1. canonicalize selected path;
2. locate manifest and Git boundary;
3. validate trust decision;
4. parse/validate manifest and lockfile;
5. calculate project identity;
6. detect duplicate registration;
7. inspect tools/providers without mutation;
8. write local registry transaction;
9. start watchers only after success.

Moving a project requires explicit relocation detection and repository identity match.

## 8. File watching

- Use native filesystem notifications with debounce and periodic reconciliation.
- Watch manifest, lockfile, relevant Git metadata, source roots, art pointers, and local state.
- Do not recursively watch ignored build/cache/vendor directories.
- Coalesce bursts but preserve a rescan marker if events overflow.
- Watcher events never directly trigger publication.
- Symlink changes and case-only renames are normalized and surfaced.

## 9. Process supervision

### Git

Commands use explicit executable resolution, working directory, argument array, clean environment allowlist, bounded output, timeout, cancellation, and secret redaction. No shell interpretation.

### Rojo

One supervised server per workspace/project configuration. Daemon tracks PID, version, port, project hash, logs, health, and connected plugin sessions. Unexpected exit uses bounded exponential restart unless configuration or compatibility caused failure.

### Studio

Daemon discovers signed Roblox Studio installations, records version/channel, and launches only documented argument sets. It does not kill unrelated Studio processes. Runner processes get operation-specific environment and loopback token.

## 10. Operations engine

- Durable state follows SPEC-02.
- Work is split into idempotent steps with checkpoints.
- A step declares inputs, outputs, retry class, timeout, and compensation.
- Automatic retry only for classified transient errors.
- Backoff uses full jitter and respects provider retry-after.
- Waiting for user approval or Studio interaction uses `waiting-user`, not a busy loop.
- Cancellation cannot interrupt an external mutation after commit point; UI reports `cancellation-pending`/final outcome.

## 11. Local CAS

- Immutable content addressed by BLAKE3-256.
- Write to temporary file, hash while streaming, fsync as configured, atomic rename.
- Existing object is verified by size/hash sampling/full hash policy before reuse.
- Metadata DB tracks media type, size, creation/last access, verification, provider presence, references.
- Quota eviction removes only unreferenced cache objects.
- Corrupt object is quarantined and refetched; repeated corruption becomes security/health alert.

## 12. CLI command surface

Global:

```text
neuman --version
neuman doctor [--json]
neuman login roblox|github|hub
neuman logout roblox|github|hub
neuman account list|status|revoke
```

Project/config:

```text
neuman project add|remove|list|show
neuman config validate|effective|migrate|lock
neuman ownership check|explain
```

Workspace/Git/Rojo:

```text
neuman workspace status|fetch|update|open
neuman rojo start|stop|status|logs
neuman studio list|open|pair|unpair
```

Art:

```text
neuman art status
neuman art capture --place <key> [--cells ...]
neuman art propose --message <text>
neuman art review|accept|reject
neuman art apply <revision>
neuman art diff <base> <target>
neuman art lock acquire|renew|release|list
```

Build/release:

```text
neuman build create --place <key> --code <oid> --art <revision>
neuman build status|logs|cancel|verify
neuman release create --build <id> --environment <key>
neuman release approve|publish|status|rollback|resume
neuman drift inspect|capture|adopt|waive
```

Storage/admin:

```text
neuman cache status|verify|prune
neuman provider status|test
neuman support-bundle create
```

Commands that mutate production require explicit flags plus interactive confirmation unless a signed automation policy authorizes non-interactive use. `--yes` alone cannot bypass missing approval or policy gates.

## 13. CLI output

- Human output defaults to concise outcome-first text.
- `--json` emits one versioned JSON result object to stdout; logs go to stderr.
- `--jsonl` is available for streaming operations.
- Secret values are never emitted.
- Exit codes:
  - `0` success
  - `2` validation/configuration
  - `3` conflict
  - `4` authentication/authorization
  - `5` compatibility
  - `6` external unavailable/rate limited
  - `7` operation failed after mutation/partial result
  - `8` corruption/security
  - `130` cancelled/interrupted before commit point

## 14. Project trust

On first use, daemon computes repository identity and shows:

- canonical path/remote;
- requested external tools/hooks/scripts;
- provider URLs;
- manifest production targets;
- extensions.

Trust is recorded by repository identity plus manifest security-relevant hash. Security-expanding changes require renewed trust.

## 15. Repository-supplied execution

Default deny:

- Git hooks;
- arbitrary build shell commands;
- downloaded executables;
- arbitrary Studio runner Luau;
- extension code.

Trusted extensions, if added later, require signed packages, declared capabilities, sandboxing, and separate specification.

## 16. Updates

- Desktop/daemon/CLI bundle is code-signed.
- Update metadata is signed separately with rollback protection.
- Daemon downloads to staging, verifies checksum/signature, and installs only at safe point.
- Database migration backup precedes activation.
- Failed start triggers one rollback to prior compatible version and preserves diagnostics.
- Plugin and runner compatibility are checked before app update is offered as ready.

## 17. Resource limits

Defaults:

- max concurrent CPU-heavy build/index tasks: `max(1, logicalCpus/2)` configurable;
- max concurrent provider transfers: 4 per provider;
- max buffered subprocess output: 16 MiB per process with rotating spill file;
- max local IPC request: 8 MiB; larger content uses CAS references/streaming;
- max SQLite busy wait: 5 seconds then typed contention error;
- disk reserve: stop new large materialization below 5 GiB or 5%, whichever is larger.

Limits are configurable within safety bounds and visible in diagnostics.

## 18. Shutdown and crash recovery

Graceful shutdown:

1. stop accepting new mutations;
2. checkpoint operations;
3. renew or explicitly release locks according to user choice/background mode;
4. stop child processes owned by daemon if configured;
5. flush DB/logs;
6. close IPC/discovery.

On crash restart:

- verify database and temp files;
- mark running attempts interrupted;
- reconcile external side effects before retry;
- reconnect Studio/Hub;
- never assume a publish did not occur merely because response was lost.

## 19. Error codes

- `CORE_DAEMON_UNAVAILABLE`
- `CORE_VERSION_INCOMPATIBLE`
- `CORE_PROJECT_UNTRUSTED`
- `CORE_DB_BUSY`
- `CORE_DB_CORRUPT`
- `CORE_DISK_LOW`
- `CORE_OPERATION_INTERRUPTED`
- `CORE_PROCESS_FAILED`
- `CORE_OUTPUT_LIMIT`
- `CORE_UPDATE_SIGNATURE_INVALID`
- `CORE_RECOVERY_REQUIRED`

## 20. Acceptance criteria

1. Daemon survives UI restart and reconnects to running operations.
2. Single-instance and IPC ACL tests prevent another OS user from commanding daemon.
3. Crash-injection tests recover every operation step without duplicate external mutation.
4. CLI JSON output is schema-tested and secret-free.
5. Project trust blocks security-expanding manifest changes.
6. CAS atomicity/corruption/quota tests pass.
7. Subprocess invocation never passes through a shell and redacts credentials.
8. Update rollback preserves data and prior compatible executable.

