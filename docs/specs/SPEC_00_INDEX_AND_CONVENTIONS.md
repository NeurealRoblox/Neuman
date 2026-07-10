# SPEC-00 — Specification Index and Conventions

Status: Draft for implementation review  
Version: 0.1.0  
Last updated: 2026-07-09  
Supersedes: none  
Primary architecture: `/docs/architecture/ROBLOX_BUILD_MANAGER_ARCHITECTURE.md`

Architecture decisions that narrow these specifications are recorded as `ADR_NNN_*`; [ADR-001](/docs/adrs/ADR_001_ROBLOX_NATIVE_ASSEMBLY_PROFILES.md) defines the accepted Roblox native-assembly execution profiles, and [ADR-002](/docs/adrs/ADR_002_LORE_AS_OPTIONAL_ART_STORAGE.md) keeps Epic Lore as an optional measured art-storage adapter rather than v1 authority.

## 1. Purpose

This file is the root of the NeuMan specification set. It defines the normative language, document ownership model, identifiers, compatibility policy, change process, and the complete list of component and cross-system specifications.

The specification set is the product contract. Source code, UI copy, API implementations, and operational runbooks MUST conform to it. When code and a ratified specification disagree, the discrepancy MUST be treated as a defect or resolved through a specification change; undocumented behavior MUST NOT silently become the contract.

## 2. Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **NOT RECOMMENDED**, **MAY**, and **OPTIONAL** are to be interpreted as described by RFC 2119 and RFC 8174 when, and only when, they appear in uppercase.

Additional NeuMan terms:

- **Invariant** — a condition that MUST always be true in every supported state.
- **Gate** — a condition that MUST be satisfied before a state transition can occur.
- **Authority** — the only system allowed to originate changes for a declared ownership region.
- **Accepted** — reviewed or policy-approved and eligible as a build input.
- **Observed** — detected state that is not necessarily trusted or accepted.
- **Logical build** — the canonical set of inputs and policies, independent of byte-level serializer variation.
- **Release bundle** — the immutable bytes and manifests promoted through environments.
- **Implementation-defined** — behavior the implementation may choose but MUST document and keep compatible within a major version.
- **Operator-defined** — behavior explicitly selected in deployment configuration.

## 3. Scope of the product

NeuMan is an open-source Roblox development and release manager composed of:

1. a native desktop application;
2. a local daemon and CLI;
3. a Roblox Studio plugin;
4. a loopback protocol between Studio and the daemon;
5. Git, Rojo, GitHub, and Roblox integrations;
6. an optional self-hosted Hub for team coordination;
7. content-addressed art and build storage;
8. build, staging, publication, rollback, and drift workflows;
9. security, observability, backup, testing, and release infrastructure.

NeuMan does not replace Roblox Studio, Team Create, Git, Rojo, GitHub, or Roblox Open Cloud. It provides explicit contracts and orchestration between them.

The official project operates no NeuMan account system, OAuth proxy, multi-tenant Hub, central project/art database, or default telemetry collector. The desktop is local-first; optional team infrastructure is self-hosted/user-owned. Official binaries are distributed through a protected, signed, attested GitHub release workflow as specified by SPEC-20/21 and `/docs/guides/OFFICIAL_RELEASES.md`.

## 4. Specification catalog

| ID | File | Normative subject | Depends on |
|---|---|---|---|
| SPEC-00 | `/docs/specs/SPEC_00_INDEX_AND_CONVENTIONS.md` | Index, vocabulary, compatibility, governance | Architecture |
| SPEC-01 | `/docs/specs/SPEC_01_PRODUCT_BOUNDARIES_AND_INVARIANTS.md` | Goals, boundaries, actors, system invariants | SPEC-00 |
| SPEC-02 | `/docs/specs/SPEC_02_DOMAIN_MODEL_AND_STATE_MACHINES.md` | Entities, identifiers, events, lifecycle state machines | SPEC-00, SPEC-01 |
| SPEC-03 | `/docs/specs/SPEC_03_PROJECT_MANIFEST_AND_CONFIGURATION.md` | Project manifest, lockfile, local configuration | SPEC-02 |
| SPEC-04 | `/docs/specs/SPEC_04_IDENTITY_AUTHENTICATION_AND_AUTHORIZATION.md` | Identities, accounts, sessions, roles, authorization | SPEC-02, SPEC-03 |
| SPEC-05 | `/docs/specs/SPEC_05_DESKTOP_APPLICATION.md` | Desktop UX, screens, application shell, accessibility | SPEC-01–04 |
| SPEC-06 | `/docs/specs/SPEC_06_CORE_DAEMON_AND_CLI.md` | Process model, IPC, commands, local cache, supervisors | SPEC-02–04 |
| SPEC-07 | `/docs/specs/SPEC_07_STUDIO_PLUGIN.md` | Plugin UX, capture/apply behavior, Studio lifecycle | SPEC-02–04, SPEC-08–10 |
| SPEC-08 | `/docs/specs/SPEC_08_LOOPBACK_BRIDGE_PROTOCOL.md` | Discovery, pairing, messages, transfer, retry | SPEC-02, SPEC-04 |
| SPEC-09 | `/docs/specs/SPEC_09_ART_CELLS_REVISIONS_AND_DIFFS.md` | Native cell format, identity, revision graph, merge/diff | SPEC-02–03 |
| SPEC-10 | `/docs/specs/SPEC_10_TERRAIN_SERVICES_ASSETS_AND_PACKAGES.md` | Terrain, service state, external assets, packages | SPEC-02–03, SPEC-09 |
| SPEC-11 | `/docs/specs/SPEC_11_GIT_AND_ROJO_INTEGRATION.md` | Git workspaces, ownership partitions, Rojo lifecycle | SPEC-02–03, SPEC-09 |
| SPEC-12 | `/docs/specs/SPEC_12_GITHUB_APP_INTEGRATION.md` | GitHub App auth, permissions, webhooks, checks, PRs | SPEC-04, SPEC-11 |
| SPEC-13 | `/docs/specs/SPEC_13_ROBLOX_INTEGRATION.md` | OAuth, Open Cloud, Studio discovery/CLI, API policy | SPEC-03–04 |
| SPEC-14 | `/docs/specs/SPEC_14_BUILD_ENGINE.md` | Build graph, toolchain, validation, artifacts | SPEC-02–03, SPEC-09–13 |
| SPEC-15 | `/docs/specs/SPEC_15_RELEASE_PUBLISH_ROLLBACK_AND_DRIFT.md` | Environments, approvals, publishing, rollback, drift | SPEC-02, SPEC-13–14 |
| SPEC-16 | `/docs/specs/SPEC_16_HUB_CONTROL_PLANE.md` | Hub APIs, locks, presence, relay, team coordination | SPEC-02–04, SPEC-09 |
| SPEC-17 | `/docs/specs/SPEC_17_STORAGE_CAS_GIT_LFS_AND_LORE.md` | CAS, object layout, provider API, Git LFS, Lore | SPEC-02, SPEC-09, SPEC-14 |
| SPEC-18 | `/docs/specs/SPEC_18_SECURITY_AND_THREAT_MODEL.md` | Trust boundaries, threats, cryptography, secure defaults | All component specs |
| SPEC-19 | `/docs/specs/SPEC_19_OBSERVABILITY_RELIABILITY_AND_BACKUP.md` | Logs, metrics, health, SLOs, recovery, support bundles | All runtime specs |
| SPEC-20 | `/docs/specs/SPEC_20_TESTING_COMPATIBILITY_AND_RELEASE_ENGINEERING.md` | Test pyramid, matrices, gates, signing, updates | All specs |
| SPEC-21 | `/docs/specs/SPEC_21_OPEN_SOURCE_GOVERNANCE_AND_DISTRIBUTION.md` | Licensing, governance, contribution, packaging | SPEC-00, SPEC-18–20 |
| SPEC-22 | `/docs/specs/SPEC_22_TRACEABILITY_AND_ACCEPTANCE_MATRIX.md` | Requirement-to-test traceability and readiness checklist | All specs |

No implementation phase may begin until the specifications it depends on have reached at least **Accepted for Alpha** status.

## 5. Document statuses

1. **Draft** — incomplete or under review; not an implementation contract.
2. **Accepted for Spike** — sufficient for a time-boxed feasibility implementation; details may change.
3. **Accepted for Alpha** — implementation contract for alpha; backward compatibility is not promised outside stated rules.
4. **Accepted for Beta** — externally documented contract; incompatible changes require migration.
5. **Stable** — semantic-versioned compatibility contract.
6. **Deprecated** — supported for a stated migration window.
7. **Retired** — no longer supported.

Every spec MUST state its status, semantic version, last-update date, and dependencies. A status change MUST be recorded in Git history and the decision log.

## 6. Requirement identifiers

Normative requirements use stable identifiers:

```text
<area>-<three-digit-number>
```

Areas:

- `INV` product invariant
- `DOM` domain model
- `CFG` configuration
- `IAM` identity/access
- `UX` desktop UX
- `CORE` daemon/CLI
- `STU` Studio plugin
- `LBP` loopback protocol
- `ART` art model
- `TAS` terrain/assets/services
- `GIT` Git/Rojo
- `GHA` GitHub
- `RBX` Roblox
- `BLD` build
- `REL` release
- `HUB` Hub
- `STO` storage
- `SEC` security
- `OPS` operations
- `TST` testing
- `OSS` open source/distribution

Identifiers MUST never be reused. Removed requirements remain as tombstones in SPEC-22.

## 7. Canonical data conventions

Unless a spec explicitly overrides these rules:

- Human-authored project configuration uses UTF-8 YAML 1.2 with LF line endings.
- Machine-authored durable manifests use UTF-8 JSON.
- Signed or hashed JSON uses RFC 8785 JSON Canonicalization Scheme.
- Network JSON uses lower camel case field names.
- Rust identifiers MAY use idiomatic snake_case internally but serialization names MUST remain stable.
- Timestamps use RFC 3339 UTC with millisecond precision, for example `2026-07-09T22:14:03.127Z`.
- Durations in JSON are non-negative integer milliseconds and have an `Ms` suffix.
- Byte sizes in JSON are non-negative integers and have a `Bytes` suffix.
- IDs are opaque strings. Clients MUST NOT infer authorization or ordering from an ID.
- Optional absent values are omitted from JSON unless explicit `null` has domain meaning.
- Unknown fields MUST be preserved by read-modify-write tools where practical and MUST NOT cause failure within the same schema major version unless the field is security-sensitive.
- Enum values are lowercase kebab-case strings.
- Paths in manifests use `/` regardless of host operating system.
- Filesystem paths MUST be normalized only at the host boundary; repository paths are case-sensitive logical paths.
- User-visible sizes use IEC units (`KiB`, `MiB`, `GiB`).

## 8. Identifier and hash conventions

- Domain event, build, release, lock, operation, and session IDs use UUIDv7 encoded in lowercase canonical form.
- Art cell IDs created in Studio use random UUIDv4 from `HttpService:GenerateGUID(false)` and are encoded as `cell_<uuid>`.
- Terrain tile IDs use `terrain_<layer>_<x>_<y>_<z>` with signed decimal grid coordinates.
- Singleton service-state IDs use `service_<roblox-service-name>`.
- Internal content hashes use BLAKE3-256 encoded as `b3-256:<lowercase-base32-no-padding>`.
- Externally published artifact checksums additionally use SHA-256 encoded as `sha256:<lowercase-hex>`.
- Hash comparisons MUST be constant-time where a hash authenticates or authorizes content. Ordinary CAS lookup comparisons need not be constant-time.
- The algorithm identifier is part of every persisted hash. Bare digests are invalid.

Hash and canonicalization algorithms are schema-level contracts. Changing them requires a new representation version, not an in-place reinterpretation.

## 9. Compatibility policy

Every persisted object and network envelope MUST contain a schema or protocol version. Versions use `major.minor`:

- increment **major** when an old reader cannot safely interpret the object;
- increment **minor** for additive optional fields or new enum values with defined unknown-value behavior.

Readers MUST reject an unsupported major version with a typed error and remediation. Readers SHOULD accept newer minor versions, preserve unknown fields, and treat unknown enums as `unknown` unless that would weaken security or release safety.

The desktop, daemon, CLI, plugin, Hub, and runner publish a compatibility matrix. A connection MUST be rejected before mutation if the pair is outside the matrix.

## 10. Change control

A material change requires an Architecture Decision Record when it changes any of:

- source-of-truth or ownership rules;
- persisted schema;
- network protocol;
- security boundary or credential handling;
- merge semantics;
- release or rollback guarantees;
- license or distribution model;
- supported platform baseline.

The change procedure is:

1. open an issue describing the problem and affected requirement IDs;
2. draft an ADR and spec patch;
3. provide migration, compatibility, security, and rollback analysis;
4. obtain code-owner approval for affected areas;
5. update SPEC-22 tests and traceability;
6. merge specification changes before or atomically with implementation.

## 11. Durability rules

- Specifications MUST live in the same Git repository as the reference implementation.
- Every public release MUST publish the exact spec commit it implements.
- Schemas MUST have golden fixtures and compatibility tests.
- Public protocols MUST have conformance tests independent of the primary implementation.
- No critical behavior may exist only in a UI tooltip, issue discussion, or source-code comment.
- Examples are non-normative unless explicitly marked normative.
- Generated API references do not replace behavioral specifications.

## 12. Source citations and temporal claims

Claims about Roblox, GitHub, Rojo, Tauri, Lore, or another external system MUST cite primary documentation and include the date last verified when they affect feasibility or security. Integration tests, not documentation alone, determine current compatibility.

## 13. Open decisions

The canonical Phase 0 experiment IDs are shared with SPEC-20 and SPEC-22:

1. **P0-01 — Roblox PKCE:** prove public-client OAuth on Windows and macOS, including refresh, revocation, OIDC validation, and absence of a client secret.
2. **P0-02 — Resource discovery:** prove universe, place, creator, group, permission, and account-mismatch behavior through supported interfaces.
3. **P0-03 — Native cell round-trip:** establish the exact class/property/reference corpus that `SerializationService` can reconstruct and catalog every exception.
4. **P0-04 — Terrain:** select native `TerrainRegion` or NeuMan voxel encoding, with exactness, rollback, size, and compatibility evidence.
5. **P0-05 — Loopback transfer:** qualify WebSocket headers and fallback authentication, reconnect, replay protection, corruption detection, compression, and bounded 100 MB-class transfers.
6. **P0-06 — Studio runner:** prove documented `RunScript` loading, fixed-runner authentication, structured receipts, timeouts, and deterministic exit behavior.
7. **P0-07 — `SavePlaceAsync`:** prove disposable-target publication, required place setting, target identity, Team Create restrictions, prompt behavior, and ambiguous-response reconciliation.
8. **P0-08 — Rojo 7.7:** characterize build, live sync, opt-in two-way behavior, source maps, ownership exclusions, references, and MeshPart/CSG replacement behavior.
9. **P0-09 — Storage benchmark:** compare raw Git, Git LFS, S3-compatible CAS, and Lore on representative `.rbxm` changes, transfer, recovery, locking, and export.
10. **P0-10 — Determinism:** measure native `.rbxm/.rbxl` byte stability separately from semantic stability and finalize the supported verification levels.

Each experiment MUST produce a versioned report and an ADR with `accepted`, `rejected`, or `deferred` outcome before its affected Alpha capability is accepted. Additional implementation decisions, including the Tauri accessibility baseline and embedded-versus-supervised tool packaging, follow the normal ADR process even when they do not block Phase 0.

## 14. References

- `/docs/architecture/ROBLOX_BUILD_MANAGER_ARCHITECTURE.md`
- [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119)
- [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174)
- [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785)
