# SPEC-20 — Testing, Compatibility, and Release Engineering

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: all specifications

## 1. Purpose

This specification defines test layers, fixtures, Roblox/OS/tool compatibility, Phase 0 experiments, quality gates, CI, semantic versioning, signed packaging, updates, channels, rollback, and criteria for Alpha/Beta/Stable.

## 2. Test philosophy

- Invariants and schemas are executable contracts.
- State machines receive model/property tests, not only examples.
- Platform documentation is verified by integration tests.
- Native/art test corpus includes hard/opaque content.
- Failure injection covers every external commit point.
- Independent conformance implementations protect public protocols.
- No flaky test is ignored indefinitely; quarantine has owner/expiry.

## 3. Test layers

### Unit

Pure functions, parsers, policy, hashes, state transitions, path canonicalization, value codecs, diff/merge, redaction.

### Property/model

Random state/event sequences for locks, art merge, build/release saga, GC, idempotency, protocol chunks, configuration round-trip.

### Contract

Golden JSON/YAML/protocol/API fixtures across Rust/TypeScript/Luau/independent tools.

### Integration

SQLite/PostgreSQL/S3/Git/Git LFS/Rojo/GitHub mock/sandbox/Roblox disposable environment/Studio plugin/CLI.

### End-to-end

Real signed desktop-daemon-plugin plus Hub/provider in disposable projects/universes.

### Fuzz

Manifest/lock JSON/YAML, protocol JSON/base64, CAS metadata, native parsers, webhook bodies, API cursors, import/export archives.

### Chaos/failure

Process crash, network drop, duplicate/lost events, DB failover, object-store error, disk full, clock skew, provider 429/5xx, Studio crash, ambiguous publish.

### Performance

Large repos/cells/revisions/diffs, transfer throughput, Studio responsiveness, Hub throughput/locks/events, storage/GC, startup/UI.

### Security

Threat-model tests, SAST/dependency/secret scan, DAST, penetration test, tenant isolation, update/signing, redaction.

## 4. Fixture repositories

Public synthetic fixtures:

- minimal Rojo project;
- partially managed code + Studio art;
- multi-place universe topology;
- Git conflicts/LFS/submodule/case/path edge cases;
- malicious hooks/symlinks/filters;
- generated project/ownership overlap;
- Git SHA-1 and future SHA-256 format fixtures.

No private production content in public tests.

## 5. Roblox native corpus

Disposable/generated places/cells include:

- Parts/Models/Folders/attributes/tags;
- duplicate names/moves/reparents/deletes;
- welds/constraints/attachments/ObjectValues/cross-cell refs;
- MeshPart, SurfaceAppearance, CSG PartOperation;
- EditableMesh/EditableImage/BaseWrap where available;
- animations/rigs/VFX/UI;
- packages, nested packages, auto-update, local modifications;
- terrain solid/liquid/material boundaries;
- Lighting/Atmosphere/Sky/service properties;
- unknown/new classes/properties;
- forbidden/malicious scripts/capabilities;
- large/deep/wide/malformed/corrupt models.

Every corpus case states expected fidelity/index completeness/publish-method compatibility.

## 6. Compatibility matrix

Dimensions:

- NeuMan desktop/daemon/CLI version;
- plugin version/protocol;
- runner version;
- Hub API/schema;
- Windows version/architecture;
- macOS version/architecture;
- Roblox Studio channel/build;
- Roblox API schema hash;
- Rojo server/plugin version;
- Git/Git LFS version;
- GitHub.com/GHES versions;
- PostgreSQL/S3-compatible provider;
- optional Lore version.

Matrix statuses:

- `supported`
- `supported-with-limitations`
- `experimental`
- `blocked`
- `unknown`

Unknown blocks mutations needing fidelity/release proof.

## 7. Studio release qualification

On every observed new Studio build/channel:

1. generate/record API dump hash;
2. run capability probes;
3. execute native corpus capture/deserialize/reserialize/index;
4. terrain/service tests;
5. Rojo live sync/build tests;
6. Studio runner/validation;
7. disposable staging publish when safe;
8. compare prior compatibility/opaque changes;
9. publish signed compatibility record.

Until qualified, UI allows read-only/local draft based on policy but blocks protected acceptance/release requiring new build.

## 8. Phase 0 experiments and pass criteria

### P0-01 Roblox PKCE

Pass: Windows/macOS public client signs in, validates OIDC, refresh rotates/revokes, no client secret, user/group resources work.

### P0-02 Resource discovery

Pass: exact universe/place/creator/permissions obtained for user/group; wrong/unauthorized IDs fail.

### P0-03 Native cell round-trip

Pass: representative corpus deserializes in second session with required semantic/reference/dependency equivalence; unsupported classes explicitly cataloged.

### P0-04 Terrain

Pass: chosen encoding preserves solid/liquid/material/bounds across corpus and failure rollback; ADR selects profile.

### P0-05 Loopback transfer

Pass: WebSocket/header/fallback works Windows/macOS, 96 MiB chunked transfer, reconnect/loss/corruption/replay bounded memory.

### P0-06 Studio runner

Pass: documented CLI `RunScript` loads a local and published place, authenticates a fixed manifest, performs only the validation available to its command-bar context, returns a structured receipt, and exits deterministically. The qualification MUST explicitly probe and record unavailable APIs; it MUST NOT assume `SerializationService`, whose security context is `PluginOrOpenCloud`.

### P0-07 SavePlaceAsync

Pass: disposable target publish with API enabled/signed-in permission; Team Create block observed; lost response reconciliation/version evidence; interaction requirements documented.

### P0-08 Rojo 7.7

Pass: live code, syncback/two-way opt-in, Mesh/CSG replacement/reference behavior, source map, ownership exclusions characterized.

### P0-09 Storage benchmark

Pass: published reproducible Git/LFS/S3/Lore benchmark; v1 provider decision confirmed.

### P0-10 Determinism

Pass: measure raw `.rbxm/.rbxl` stability and semantic fingerprint; specs/verification levels finalized.

### P0-11 Open Cloud Luau Execution

Pass: operator-owned CI creates a task against an exact disposable place/version, supplies bounded binary native-cell inputs to a fixed runner, polls task state and bounded logs, validates a signed receipt, and proves timeout/lost-response reconciliation. A separate publish fixture proves `SavePlaceAsync` behavior and resulting place-version evidence. The API key remains only in the operator's CI secret store and is never accepted, proxied, logged, or persisted by the public desktop, Studio plugin, or Hub.

Failure requires ADR/architecture/spec change before Alpha, not waiver.

## 9. CI pipeline

For every PR:

1. spec/schema lint and link checks;
2. format/lint/type checks Rust/TS/Luau;
3. unit/property/contract tests;
4. dependency/license/secret/security scan;
5. targeted integration tests;
6. build packages unsigned/test-signed;
7. artifact/SBOM/provenance generation;
8. compatibility impact label and required owner review.

Main/nightly:

- full integration/fuzz budget;
- PostgreSQL/S3/Git/Git LFS/Rojo matrices;
- Studio disposable matrix on available Windows/macOS runners;
- performance regression;
- backup/restore subsets;
- update/rollback tests.

Release candidate:

- all supported platform/Studio matrices;
- E2E staging/publish/rollback in disposable universe;
- penetration/security gates;
- signed packaging/update rehearsal;
- docs/spec traceability.

## 10. Test isolation

- Unique temp dirs/repos/buckets/DB schemas/Roblox disposable targets.
- No production credentials/targets in test configuration.
- CI environment guard rejects production-impact IDs.
- Parallel tests use unique ports/resources.
- Cleanup idempotent but preserves failed artifacts/logs within retention.

## 11. Flaky tests

Flake record includes owner, issue, first seen, rate, quarantine scope, expiry ≤14 days. Quarantined release-critical test blocks release or requires release-owner/security approval with risk; it is not silently skipped.

## 12. Coverage

Targets are risk-based, not only percentage:

- 100% state-transition branches and authorization decisions;
- golden/negative every schema/protocol message;
- failure injection every external commit point;
- property tests merge/root/idempotency/GC;
- privileged/security module high line/branch target ≥90%; other core ≥80% guideline;
- UI critical flows E2E/accessibility.

## 13. Performance baselines

Published baseline hardware/data. Regression budgets:

- desktop startup/UI SPEC-05;
- plugin scanning/apply responsiveness;
- 1/20/96 MiB transfers;
- 1k/10k cells/revision diff;
- 1GB/100k object CAS/GC scenarios;
- Hub 1k concurrent sessions/locks initial target, adjusted by product need;
- build small/medium/large fixture.

>10% unexplained regression in critical metric blocks release or needs documented acceptance.

## 14. Semantic versioning

All products use SemVer, coordinated compatibility matrix:

- major: incompatible public protocol/schema/CLI behavior;
- minor: backward-compatible feature;
- patch: compatible fixes/security.

Pre-1.0 may change, but persisted data always has migration/export path and public beta changes require notes.

Plugin/runner protocol versions are separate from package version. Hub API supports at least current and previous compatible desktop minor during rolling upgrade policy.

## 15. Release channels

- `nightly` — automated, development only, no production targets.
- `alpha` — testers, breaking changes/migrations possible.
- `beta` — signed, supported migration, public testing.
- `stable` — strongest compatibility/SLO.

Separate signing/update metadata per channel. Downgrade across schema incompatibility blocked unless safe rollback path.

## 16. Packaging

Windows:

- signed installer/executable;
- per-user install default;
- clean uninstall preserving user data by choice;
- Windows reputation/signature validation.

macOS:

- signed/notarized app bundle/installer;
- hardened runtime/entitlements minimum;
- quarantine/update behavior tested.

CLI/Hub:

- signed checksums/releases;
- Hub OCI image multi-arch with non-root user/read-only root where practical;
- SBOM/provenance.

Plugin:

- Creator Store and checksumed GitHub release/local package;
- version/source/build reproducibility;
- no obfuscated/minified-only source for open-source distribution.

## 17. Signing and release

- Official binary distribution is the canonical public GitHub repository's Releases page; maintainers MUST NOT upload or replace official assets from a workstation.
- The GitHub workflow is manual-dispatch only and requires a protected, annotated, GitHub-verified signed tag plus a separately entered full commit SHA.
- Signing and publication use separate protected GitHub environments with required reviewers. Untrusted pull requests, forks, branches, and `pull_request_target` MUST NOT reach either environment or any signing credential.
- Every referenced GitHub Action is pinned to a full commit SHA. Job permissions are least-privileged; only attestation jobs receive `id-token: write`/`attestations: write`, and only draft/publish jobs receive `contents: write`.
- Rust, Node, Cargo, npm, source tag, package manifests, and lockfiles are pinned/consistent. CI builds from a clean checkout of the resolved commit.
- The official public Roblox `client_id` is injected at compile time from reviewed GitHub environment configuration. It is public; no client secret exists.
- Windows MSI/EXE assets require a valid Authenticode signature from the declared certificate plus a trusted SHA-256 timestamp. The workflow verifies signer thumbprint, status, and timestamp before upload.
- macOS app/DMG assets require Developer ID signing, hardened distribution configuration, Apple notarization, Gatekeeper assessment, and stapled-ticket validation before upload.
- Tauri updater bundles require a separate updater signature. The updater private key is not the platform certificate, and update metadata is not promoted before every asset/signature is present.
- Each published file has an aggregate SHA-256 entry, a GitHub OIDC/Sigstore artifact attestation bound to the canonical repository/workflow/commit, and a record in `release-evidence.json`.
- The workflow first creates an immutable draft release. A second protected publish job redownloads the GitHub assets, verifies checksums and repository-bound attestations, then publishes after approval.
- Generate and attest an SBOM before public beta. Verify packages, signatures, SBOM, provenance, updater metadata, and clean-install/update behavior in clean VMs before promotion.
- Publish release notes, compatibility, known limitations, migrations, signer identities, updater-key identity, and consumer verification commands.
- Maintain documented key rotation, revocation, compromised-release withdrawal, emergency reinstall, and no-same-tag-replacement processes.

The full normative key inventory, workflow contract, evidence set, consumer commands, and incident procedure are in `/docs/guides/OFFICIAL_RELEASES.md`. `/.github/workflows/ci.yml` and `/.github/workflows/official-release.yml` are repository-bootstrap sources for `.github/workflows/ci.yml` and `.github/workflows/official-release.yml`; they MUST be copied byte-for-byte into those GitHub-recognized paths before any build is called official.

## 18. Updater tests

- valid update;
- corrupted/truncated;
- wrong signature/key/channel/platform;
- rollback/freeze attack;
- interrupted download/install;
- app running operation/lock;
- DB migration failure/rollback;
- plugin incompatible;
- offline startup after failed update.

## 19. Alpha gate

- Phase 0 ADRs complete.
- One artist/developer/place vertical slice.
- No unresolved critical security/data loss defect.
- Specs 00–18 Accepted for Alpha.
- Backup/export of local authoritative data.
- Signed internal packages.
- Known limitations visible.

## 20. Team alpha gate

- Hub/locks/event/object tests.
- Cross-machine artist-to-developer flow.
- Multi-place staging saga and rollback drill.
- Tenant/authz suite.
- Hub restore drill.
- Windows/macOS supported matrix.

## 21. Public beta gate

- Roblox OAuth app review/compliance.
- Creator Store plugin.
- External security assessment.
- Signed/notarized update chain.
- Self-host docs/export/restore.
- SLO dashboards/runbooks/support policy.
- No P0/P1 known data integrity/security issue.
- Specs/traceability Accepted for Beta.

## 22. Stable gate

- six months beta usage and migration evidence;
- backwards compatibility commitments;
- recovery/upgrade reliability;
- public protocol conformance suite;
- performance/capacity targets;
- governance/release cadence functioning.

## 23. Acceptance criteria

1. Phase 0 results resolve every declared open feasibility decision.
2. Compatibility records gate unknown Studio/tool versions before mutation.
3. CI cannot access production targets/credentials from untrusted PR.
4. Release packages/signatures/SBOM/provenance verify independently.
5. Failure/chaos/security/performance suites cover critical contracts.
6. Update rollback and schema migration drills pass.
7. SPEC-22 has no unmapped must-level requirement at release.
