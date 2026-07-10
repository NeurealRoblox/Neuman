# NeuMan documentation

This directory contains the durable engineering record for NeuMan. Product-facing orientation stays in the repository root; detailed contracts live here.

## Start here

1. [Architecture overview](architecture/ROBLOX_BUILD_MANAGER_ARCHITECTURE.md)
2. [Specification index and vocabulary](specs/SPEC_00_INDEX_AND_CONVENTIONS.md)
3. [Current implementation status](status/IMPLEMENTATION_STATUS.md)

## Architecture decisions

- [ADR-001: Roblox native assembly profiles](adrs/ADR_001_ROBLOX_NATIVE_ASSEMBLY_PROFILES.md)
- [ADR-002: Lore as optional art storage](adrs/ADR_002_LORE_AS_OPTIONAL_ART_STORAGE.md)

## Operational guides

| Area | Guide |
| --- | --- |
| Desktop | [Desktop application](guides/DESKTOP_README.md) |
| Core | [Core implementation](guides/CORE_IMPLEMENTATION.md) |
| Git and Rojo | [Git/Rojo integration](guides/GIT_ROJO_IMPLEMENTATION.md) |
| Studio | [Studio bridge](guides/STUDIO_BRIDGE_README.md) · [Studio runner](guides/STUDIO_RUNNER_README.md) |
| Roblox | [OAuth refresh](guides/ROBLOX_OAUTH_REFRESH.md) · [Resource provider](guides/ROBLOX_RESOURCE_PROVIDER.md) |
| Hub | [Hub service](guides/HUB_README.md) · [Desktop adapter](guides/HUB_DESKTOP_ADAPTER.md) |
| GitHub | [GitHub App integration](guides/GITHUB_APP_IMPLEMENTATION.md) |
| Releases | [Official release contract](guides/OFFICIAL_RELEASES.md) |
| Configuration | [Rojo desktop configuration](guides/ROJO_DESKTOP_CONFIG.md) |

## Specifications

The normative specification set is under [`specs/`](specs). Use [SPEC-00](specs/SPEC_00_INDEX_AND_CONVENTIONS.md) for ordering, terminology, and dependencies.

Specifications describe the intended complete system. They are not, by themselves, claims that a feature is production-ready; the [implementation status](status/IMPLEMENTATION_STATUS.md) is authoritative for executable coverage and qualification gaps.
