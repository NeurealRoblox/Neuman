# NeuMan

NeuMan is an open-source, local-first Roblox build manager. It is designed to connect Roblox identity, Git source, Studio-native art, deterministic builds, and controlled releases without requiring a vendor-operated project database.

> **Project status:** early development. The current desktop milestone focuses exclusively on a high-quality Roblox OAuth connection. The wider build, Studio, Hub, and release systems are implemented in varying stages and are not yet qualified for production use.

[Documentation](docs/README.md) · [Implementation status](docs/status/IMPLEMENTATION_STATUS.md) · [Security policy](SECURITY.md) · [Contributing](CONTRIBUTING.md)

## What NeuMan is building

- Roblox first-party OAuth through a public client, PKCE, and OS-protected tokens.
- Git and Rojo synchronization with explicit ownership boundaries.
- A paired Studio plugin for live code and Studio-native art workflows.
- Immutable art revisions instead of pretending RBXM files can be text-merged.
- Deterministic build manifests, staged publication, verification, and rollback.
- An optional self-hosted Hub for teams that need remote coordination.

NeuMan operates no hosted account service, OAuth proxy, central project database, or default telemetry collector. Production is a deployment target—not a source of truth.

## Repository layout

| Path | Purpose |
| --- | --- |
| [`docs/`](docs/README.md) | Architecture, ADRs, specifications, guides, and status |
| [`.github/`](.github) | Active workflows and ownership rules |
| `*.rs` | Rust core, CLI, desktop boundary, bridge, Hub, and providers |
| [`desktop.tsx`](desktop.tsx) | Focused React desktop experience |
| [`studio_plugin.luau`](studio_plugin.luau) | Roblox Studio plugin |
| [`Cargo.toml`](Cargo.toml) | Rust package and binary definitions |
| [`package.json`](package.json) | Frontend and Tauri commands |

The Rust crate is intentionally flat during the bootstrap phase. A later internal refactor can move modules under `src/` without changing public protocols.

## Build and test

Prerequisites are pinned in the repository: Rust 1.93 and Node.js 24.12.

```text
npm ci
npm run build
npm run check:studio

cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings

cargo test --locked --features desktop --bin neuman-desktop
cargo clippy --locked --features desktop --bin neuman-desktop -- -D warnings
```

Run the desktop in development:

```text
npm run dev:desktop
```

Build a standalone unsigned development executable:

```text
npm run build:desktop:debug
```

A source build that exercises Roblox login must compile with `NEUMAN_ROBLOX_OAUTH_CLIENT_ID` set to the registered public client ID. A client secret is never accepted or embedded.

## Documentation

Start with the [documentation index](docs/README.md). The short version is:

- [System architecture](docs/architecture/ROBLOX_BUILD_MANAGER_ARCHITECTURE.md)
- [Specification index](docs/specs/SPEC_00_INDEX_AND_CONVENTIONS.md)
- [Architecture decisions](docs/adrs)
- [Desktop guide](docs/guides/DESKTOP_README.md)
- [Official release contract](docs/guides/OFFICIAL_RELEASES.md)
- [Current implementation status](docs/status/IMPLEMENTATION_STATUS.md)

## Security and releases

OAuth tokens stay in Windows Credential Manager or macOS Keychain. NeuMan does not use `.ROBLOSECURITY`, does not place Roblox operator API keys in the desktop renderer or Studio plugin, and fails closed when required evidence is unavailable.

Official releases must come from the protected workflows under [`.github/workflows`](.github/workflows), with native signing, updater signing, checksums, and GitHub artifact attestations. Release preflight intentionally remains blocked until the real updater public key and platform signing environments are configured.

## License

Apache-2.0. See [LICENSE](LICENSE).
