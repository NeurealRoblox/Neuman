# NeuMan

NeuMan is an open-source Roblox code, art, build, and release manager. The repository contains a runnable Rust CLI and core, a native Tauri/React desktop, an authenticated Roblox Studio plugin bridge, an optional self-hosted Hub reference service, fail-closed Git/Rojo orchestration, a GitHub App security boundary, and the durable specification set that governs them.

The current native-assembly decision is recorded in [ADR-001](ADR_001_ROBLOX_NATIVE_ASSEMBLY_PROFILES.md): the paired Studio plugin is the public desktop authority, Open Cloud Luau Execution is an operator-owned CI profile, and CLI `RunScript` is limited to separately qualified validation/launch behavior.

[ADR-002](ADR_002_LORE_AS_OPTIONAL_ART_STORAGE.md) records the storage decision: Epic Lore is a strong optional CAS/provider candidate and must be benchmarked, but it is not the v1 authority or a substitute for Roblox-aware art revisions, leases, and review.

NeuMan has no vendor-operated control plane, account system, or central project database. The desktop talks directly to Roblox and Git/GitHub, and stores project state in user-owned local files, SQLite, and content-addressed storage. Teams that need remote locks, presence, or accepted-art fan-out deploy and own the optional open-source Hub themselves. GitHub Releases is the official binary distribution and provenance channel; it is not a runtime dependency or project-data service.

The authority model is intentionally strict:

- Git commits are the source of truth for code and committed configuration.
- Accepted content-addressed art revisions are the source of truth for Studio-native content.
- Production is a deployment target, never an editing source.
- Builds resolve every input into an immutable manifest and release bundle.
- Binary art is not text-merged. Protected leases, explicit proposals, immutable snapshots, and user-previewed replacement preserve intent.

Start with [SPEC_00_INDEX_AND_CONVENTIONS.md](SPEC_00_INDEX_AND_CONVENTIONS.md). The architecture and all 22 subsystem specifications are normative; [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md) records the current executable coverage and qualification gaps.

## Implemented systems

- `domain.rs` and `core.rs`: typed IDs, RFC 8785 canonicalization, BLAKE3/SHA-256 identities, manifest and ownership validation, SQLite ledger, content-addressed storage, immutable art/build/bundle records, release preflight, and guarded Roblox publication.
- `git_rojo.rs` and `rojo_desktop_config.rs`: hostile-config-hardened Git sync, exact commits, isolated worktrees, manifest/lock-bound Rojo capabilities, strict recursive project ownership preflight, and one managed loopback process per canonical workspace/place.
- `bridge.rs` and `studio_plugin.luau`: explicit pairing, scoped local credentials, generation-bound transfers, CAS/ledger-before-ACK capture, stale-base rejection, exact-context accepted fan-out, authenticated large downloads, dirty-cell protection, and undoable Studio replacement.
- `studio_runner.rs` and `native_execution.rs`: signed profile-specific manifests/receipts, fixed-runner identity, one-time authentication, exact provider task/version evidence, and a crash-safe key-free operator state machine; they never accept repository-supplied runner code or serialize an API key.
- `hub.rs` and `hub_desktop.rs`: user-owned roles/storage, server-recomputed canonical art state and leases, proposal/head CAS, object transfers, durable events/cursors, native cross-machine import/fan-out, presence, build/release evidence, audit, quotas, and retention.
- `github_app.rs`: webhook authentication and replay protection, App JWTs, repository-scoped installation tokens, numeric repository binding, check runs, retry classification, and authenticated PR markers.
- `roblox_oauth.rs` and `roblox_resources.rs`: public-client-only refresh rotation and read-only universe/place inventory with pinned origins, bounded transports, mandatory rotation, OAuth grant/subject/path validation, and secret-safe errors.
- `neuman_desktop.rs`, `desktop.tsx`, and `desktop.css`: native command boundary, embedded Studio bridge and explicit pairing UI, fixed-loopback Roblox OAuth with PKCE and fail-closed OS-vault token storage/rotation, vault-only read-only Roblox universe/place inventory and provider evidence, Git sync, pinned Rojo session controls, exact art/build/release forms, and operation activity.

This checkout uses a flat single-crate layout. The binaries are:

- `neuman` — local CLI and automation JSON interface;
- `neuman-bridge` — standalone diagnostic bridge host;
- `neuman-hub` — self-hosted coordination service;
- `neuman-desktop` — optional-feature native desktop.

## Build and test

Prerequisites: the repository-pinned Rust 1.93 toolchain, Node.js 24.12, system Git, and—when exercising live synchronization—a checksum-pinned compatible Rojo and Roblox Studio.

```text
cargo test --locked --all-targets
cargo run --bin neuman -- --help
cargo run --bin neuman-bridge
cargo run --bin neuman-hub

npm ci
npm run check:studio
npm run build
cargo test --locked --features desktop --bin neuman-desktop
cargo clippy --locked --features desktop --bin neuman-desktop -- -D warnings
npm run dev:desktop
npm run build:desktop:debug
```

`build:desktop:debug` is the supported standalone development build. Its
runner arguments select `neuman-desktop` instead of the sibling `neuman` CLI.
Running
`cargo build` directly creates a Tauri development executable that expects the
Vite server at `127.0.0.1:1420`; it is not a distributable desktop build.

Source builds that exercise Roblox sign-in set `NEUMAN_ROBLOX_OAUTH_CLIENT_ID` at compile time to their registered public-client ID. Official GitHub builds compile in the project's public ID from the protected release environment. A client secret is neither accepted nor embedded.

The desktop build emits a deterministic placeholder icon from `build.rs` so a source checkout remains text-only. Release packaging must replace it with signed brand assets.

## First local project

```text
neuman init --slug my-game --name "My Game" --root C:\Projects\my-game
neuman --root C:\Projects\my-game project validate --json
neuman --root C:\Projects\my-game code status --json
neuman --root C:\Projects\my-game code sync --remote origin --json
```

Configure ownership, Roblox targets, art channels, and a pinned toolchain before building. The core will not infer an art revision, production target, approval, permission proof, predecessor, drift result, lease, or staging proof.

See [CORE_IMPLEMENTATION.md](CORE_IMPLEMENTATION.md), [GIT_ROJO_IMPLEMENTATION.md](GIT_ROJO_IMPLEMENTATION.md), [STUDIO_BRIDGE_README.md](STUDIO_BRIDGE_README.md), [HUB_README.md](HUB_README.md), [GITHUB_APP_IMPLEMENTATION.md](GITHUB_APP_IMPLEMENTATION.md), [ROBLOX_RESOURCE_PROVIDER.md](ROBLOX_RESOURCE_PROVIDER.md), and [DESKTOP_README.md](DESKTOP_README.md) for subsystem operation.

## Security boundaries

- No `.ROBLOSECURITY` browser cookie is requested or accepted.
- User sign-in is Roblox authorization code with S256 PKCE. Register exactly `http://localhost:43891/oauth/callback`; the listener binds only numeric loopback.
- OAuth tokens remain in the operating-system credential vault. Roblox operator API keys never enter the renderer, Studio plugin, project files, or command arguments.
- Supported Windows desktop builds use Credential Manager and supported macOS desktop builds use Keychain. If the protected backend is unavailable or locked, token persistence fails closed; NeuMan never falls back to plaintext. Linux remains CLI/Hub-only until Roblox Studio and a qualified desktop credential profile are supported.
- Git and Rojo are invoked as argument arrays without a shell. Git hooks are disabled command-locally and global configuration is never changed silently.
- GitHub App private keys, webhook secrets, and installation tokens are Hub-only secrets.
- The Studio plugin receives only a scoped local bridge credential. It never receives Roblox, GitHub, Hub, object-store, or release credentials.
- Unknown external state, drift, permissions, hashes, predecessors, or approval evidence block mutation.

Official installers must be built only by the protected GitHub Actions release workflow. Windows packages require Authenticode, macOS packages require Developer ID signing plus notarization/stapling, updater artifacts require the independent Tauri updater signature, and every published asset receives a GitHub/Sigstore provenance attestation and SHA-256 entry. The reviewed root workflow sources still must be materialized under `.github/workflows` in a normal checkout. See [OFFICIAL_RELEASES.md](OFFICIAL_RELEASES.md).

Contribution, disclosure, conduct, governance, ownership, and release-history policies are in [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md), [GOVERNANCE.md](GOVERNANCE.md), [CODEOWNERS](CODEOWNERS), and [CHANGELOG.md](CHANGELOG.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
