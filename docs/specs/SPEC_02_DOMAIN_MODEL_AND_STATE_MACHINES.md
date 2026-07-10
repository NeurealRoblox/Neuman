# SPEC-02 — Domain Model and State Machines

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-00, SPEC-01

## 1. Purpose

This specification defines canonical NeuMan entities, identifiers, relationships, events, concurrency rules, and lifecycle state machines. Component-local models MUST map to these concepts without changing their semantics.

## 2. Primitive types

```text
UuidV7          lowercase canonical UUIDv7
UuidV4          lowercase canonical UUIDv4
ProjectId       "prj_" + UuidV7
WorkspaceId     "wsp_" + UuidV7
SessionId       "ses_" + UuidV7
ArtRevisionId   "art_" + UuidV7
BuildId         "bld_" + UuidV7
ReleaseId       "rel_" + UuidV7
OperationId     "op_"  + UuidV7
LockId          "lck_" + UuidV7
CellId          "cell_" + UuidV4
ContentHash     "b3-256:" + base32 digest
Sha256          "sha256:" + lowercase hex digest
UtcTimestamp    RFC 3339 UTC milliseconds
GitOid          lowercase 40-hex SHA-1 or 64-hex SHA-256 with repository objectFormat recorded
RobloxId        unsigned decimal integer encoded as a JSON string
RepositoryPath  normalized `/`-separated UTF-8 logical path
DataModelPath   `/`-separated Roblox instance name path with escaping defined in SPEC-03
```

Roblox numeric IDs MUST be strings in durable JSON to avoid JavaScript precision loss. UI layers MAY format them as numbers but MUST preserve exact digits.

## 3. Core entities

### 3.1 Project

Fields:

- `projectId: ProjectId`
- `slug: string` — globally unique within one Hub deployment; lowercase `[a-z0-9-]{3,63}`
- `displayName: string` — 1–100 Unicode scalar values
- `manifestPath: RepositoryPath`
- `repositoryIdentity: RepositoryIdentity`
- `createdAt`, `updatedAt`
- `status: active | archived | deleting`
- `defaultArtChannelId`
- `policyRevisionHash`
- `hubProjectId?`

Archiving is reversible and read-only. Deletion is a privileged, asynchronous operation with retention/tombstone rules.

### 3.2 RepositoryIdentity

- `provider: github | generic-git | local`
- `canonicalRemoteUrl?` — normalized HTTPS identity URL; credentials stripped
- `githubRepositoryId?` — GitHub numeric ID as string
- `objectFormat: sha1 | sha256`
- `defaultBranch`

Remote URL alone MUST NOT be used as a stable GitHub identity when repository ID is available.

### 3.3 PlaceBinding

- `placeKey` — manifest-local stable key
- `displayName`
- `authoring: RobloxTarget?`
- `environments: map<EnvironmentKey, RobloxTarget>`
- `ownershipRoots: OwnershipRoot[]`
- `baseTemplateRef`
- `releasePolicyRef`

### 3.4 RobloxTarget

- `universeId: RobloxId`
- `placeId: RobloxId`
- `creatorType: user | group`
- `creatorId: RobloxId`
- `expectedName?`
- `role: authoring | sandbox | staging | production | canary`

The IDs are authoritative. Expected name is a human-safety check, not identity.

### 3.5 Workspace

A Workspace is a local projection of one project and branch.

- `workspaceId`
- `projectId`
- `rootPath` — host path, local only
- `gitBranch`
- `gitHead`
- `upstreamRef?`
- `selectedPlaceKey?`
- `selectedArtRevisionId?`
- `lastAppliedArtStateHash?`
- `rojoSession?`
- `studioSessions: SessionId[]`
- `dirtySummary`
- `status`

Workspace status:

```text
uninitialized -> ready <-> updating
                     |        |
                     v        v
                  conflicted  error
                     |        |
                     +------> ready
ready -> closing -> closed
```

`conflicted` is a first-class recoverable status and MUST NOT be mapped to generic error.

### 3.6 OwnershipRoot

- `rootId`
- `dataModelPath`
- `owner: git-code | studio-art | terrain | service-state | generated | external-package`
- `cellStrategy: none | root | children | tagged | spatial`
- `includePatterns[]`
- `excludePatterns[]`
- `policyRef?`

Resolved ownership is evaluated against normalized DataModel identity, not string prefix alone. Overlap after expansion is invalid.

### 3.7 ArtCell

- `cellId`
- `placeKey`
- `rootId`
- `displayName`
- `logicalParentId?`
- `kind: model | folder | ui | vfx | rig | prop-set | map-zone | other`
- `currentSnapshotHash?`
- `deletedAt?`
- `attributesPolicy`

Cell identity survives name, parent, and transform changes. Deleting a cell creates a tombstone in the next art revision.

### 3.8 CellSnapshot

- `contentHash` — hash of exact native bytes
- `sizeBytes`
- `mediaType: application/x-roblox-rbxm`
- `serializationVersion`
- `studioBuild`
- `apiSchemaHash`
- `capturedAt`
- `capturedBy`
- `semanticIndexHash`
- `metadataHash`
- `previewHashes[]`
- `dependencyManifestHash`
- `externalReferenceTableHash`
- `validationSummary`

CellSnapshot is immutable. A correction creates a new snapshot.

### 3.9 ArtRevision

- `artRevisionId`
- `projectId`
- `channelId`
- `parents: ArtRevisionId[]` — zero for genesis, one normal, two for merge
- `stateRootHash` — Merkle root of complete sorted cell/service/terrain state
- `changes[]` — additions, updates, deletions relative to first parent
- `author`
- `message`
- `createdAt`
- `sourceSessions[]`
- `status`
- `validationSummary`
- `approvalSet`
- `signature?`

Statuses:

```text
capturing -> proposed -> validating -> review-required -> accepted
    |           |           |               |
    v           v           v               v
  failed      rejected    rejected        rejected

accepted -> superseded
```

Accepted revisions are immutable. `superseded` means a newer accepted head exists; the revision remains buildable.

### 3.10 TerrainTileSnapshot

- `tileId`
- `layer`
- `gridCoordinate {x,y,z}`
- `regionMin`, `regionMax`
- `resolutionStuds`
- `contentHash`
- `encoding`
- `studioBuild`, `apiSchemaHash`
- `materialPaletteHash?`
- `validationSummary`

### 3.11 ServiceStateSnapshot

- `serviceStateId`
- `serviceName`
- `schemaVersion`
- `properties` — typed canonical values
- `childCellIds[]`
- `contentHash`
- `validationSummary`

### 3.12 DependencyManifest

- `manifestHash`
- `assets[]`
- `packages[]`
- `tools[]`
- `generatedAt`

Asset dependency fields are defined in SPEC-10.

### 3.13 ToolchainLock

- `lockVersion`
- `neumanVersion`
- `pluginVersion`
- `runnerVersion`
- `rojoVersion` and checksum
- `studioChannel`
- `studioBuildConstraint`
- `apiSchemaHash`
- auxiliary tool versions/checksums

### 3.14 LogicalBuild

- `buildId`
- `logicalBuildHash` — hash of canonical inputs, not output bytes
- `projectId`, `placeKey`
- `codeRevision`
- `artRevisionId`, `artStateRootHash`
- `baseTemplateHash`
- `dependencyManifestHash`
- `toolchainLockHash`
- `policyRevisionHash`
- `requestedBy`, `requestedAt`
- `status`
- `validationSummary`
- `releaseBundleHash?`

Build states:

```text
queued -> resolving -> materializing -> validating -> assembling -> testing -> succeeded
   |          |              |              |            |          |
   +----------+--------------+--------------+------------+--------> failed

queued|resolving|materializing -> cancelled
succeeded -> expired (retention only; identity remains)
```

Every transition records an operation event. Retrying creates an attempt under the same build only if canonical inputs are unchanged; otherwise it creates a new build.

### 3.15 ReleaseBundle

- `bundleHash`
- `logicalBuildHash`
- `manifestHash`
- `artifactSet[]`
- `runnerInputHash?`
- `createdAt`
- `signatures[]`

The bundle is immutable and environment-neutral. Environment credentials and target IDs are not embedded except allowed target constraints.

### 3.16 Release

- `releaseId`
- `projectId`
- `environmentKey`
- `bundleHash`
- `placePlans[]`
- `requestedBy`
- `approvalPolicySnapshot`
- `approvals[]`
- `status`
- `createdAt`, `startedAt?`, `finishedAt?`
- `rollbackPlan`

Release state:

```text
draft -> awaiting-approval -> approved -> staging -> staged -> verifying -> verified
  |             |               |          |         |          |
  v             v               v          v         v          v
cancelled     rejected        cancelled   failed    failed     failed

verified -> publishing -> published
                |   |
                |   +-> partially-published -> rollback-required
                +----> failed

published -> rollback-requested -> rolling-back -> rolled-back
                                      |               |
                                      +-------------> rollback-failed
```

For a staging-only release, `verified` MAY be terminal. Production release cannot skip the configured staging proof.

### 3.17 PlaceReleasePlan

- target `RobloxTarget`
- publication order
- method `studio-assisted | open-cloud | manual-handoff`
- expected current deployment marker
- expected drift status
- preflight gates
- previous deployment
- result `RobloxDeployment?`

### 3.18 RobloxDeployment

- universe/place IDs
- observed place version number
- publication method
- publishedAt
- publishing actor/principal
- logical build hash
- bundle hash
- raw response evidence hash
- server restart operation IDs[]

### 3.19 DriftObservation

- `observationId`
- target
- `expectedDeployment?`
- `observedVersion?`
- `observedContentRoot?`
- `source: oauth-api | version-history | studio-capture | release-marker`
- `confidence: authoritative | strong | weak | unknown`
- `status: clean | version-drift | content-drift | unknown`
- `observedAt`

Unknown MUST remain unknown; it cannot be coerced to clean.

### 3.20 LockLease

- `lockId`
- `projectId`
- `resourceType: art-cell | terrain-tile | service-state | release`
- `resourceId`
- `channelId`
- `baseRevisionId`
- `holderPrincipalId`
- `holderSessionId`
- `acquiredAt`
- `expiresAt`
- `renewalCounter`
- `status`

Lock state:

```text
requested -> held -> releasing -> released
     |         |          |
     v         v          v
   denied    expired    released
               |
               +-> recovered (administrative record; not same lease)
```

Expiry never grants acceptance of stale work. Acceptance checks base revision and current head independently of lock history.

## 4. Principals and identities

`Principal` is the authorization subject:

- human user linked to GitHub/Roblox identities;
- service account;
- local unlinked user in local mode;
- Studio session acting through its paired daemon.

External account identifiers are attributes of a principal, not the principal primary key. Account linking is explicit and audited.

## 5. Typed values

Service state and semantic indexes encode Roblox values as tagged objects:

```json
{ "type": "vector3", "value": [1.0, 2.0, 3.0] }
{ "type": "color3", "value": [0.1, 0.2, 0.3] }
{ "type": "enum", "enumType": "Material", "value": "Grass" }
{ "type": "ref", "cellId": "cell_...", "instanceId": "inst_..." }
```

The complete value registry is versioned with the API schema. Floating-point values preserve IEEE-754 bit identity in machine-authored canonical data using a defined hexadecimal representation when JSON decimal round-trip is unsafe.

## 6. Events

All durable mutations emit a domain event:

- `eventId`
- `eventType`
- `schemaVersion`
- `projectId?`
- `aggregateType`, `aggregateId`
- `aggregateVersion` — positive monotonic integer
- `actorPrincipalId`
- `correlationId`
- `causationId?`
- `occurredAt`
- `payload`
- `payloadHash`

Event consumers MUST be idempotent by `eventId`. Aggregate writes use optimistic concurrency on `aggregateVersion`. Out-of-order events MUST be buffered/retried or rejected; they MUST NOT be applied speculatively.

## 7. Operations

Long-running work exposes an Operation:

- `operationId`
- `kind`
- `state: pending | running | waiting-user | retrying | succeeded | failed | cancelled`
- `progress {completed,total,unit,message}`
- `attempt`
- `startedAt`, `updatedAt`, `finishedAt?`
- `resultRef?`
- `error?`
- `cancellable`

Progress is advisory; state and result are authoritative. Cancellation is cooperative unless a component spec defines stronger behavior.

## 8. Error model

Typed errors contain:

- stable `code` such as `ART_CELL_DIRTY`;
- user-safe `message`;
- `category: validation | conflict | authorization | authentication | compatibility | unavailable | rate-limit | corruption | internal`;
- `retryable`;
- `retryAfterMs?`;
- `correlationId`;
- structured `details` with no secrets;
- optional remediation action IDs.

Error codes are versioned public API. Raw external error bodies are stored only in redacted diagnostic evidence.

## 9. Concurrency rules

- Durable aggregate writes use compare-and-swap on aggregate version.
- Artifact writes are content-addressed and immutable; duplicate writes are success if bytes match.
- Release and publication commands require idempotency keys.
- The same idempotency key with different canonical request content is an error.
- Locks reduce conflicts but never replace base-revision validation.
- UI optimistic updates MUST visibly indicate pending state and roll back on authoritative rejection.

## 10. Retention and tombstones

- Accepted art revisions, logical builds referenced by releases, release bundles, deployments, and audit events MUST NOT be garbage-collected while referenced.
- Unaccepted snapshots MAY expire by policy after at least 30 days.
- Deleted cells remain tombstoned in revision history.
- Account deletion pseudonymizes actor display data where legally required without invalidating audit integrity.
- CAS garbage collection uses mark-and-sweep from durable roots and a quarantine window of at least seven days.

## 11. Acceptance criteria

1. Golden JSON fixtures exist for every entity and state transition.
2. State machines reject illegal transitions.
3. UUID, Roblox ID, Git OID, and content hash parsing are fuzz tested.
4. Event reprocessing is idempotent.
5. Concurrency tests prove stale aggregate writes and stale art bases are rejected.
6. Unknown drift cannot pass a no-drift release gate.
7. Retention tests prove referenced release artifacts survive garbage collection.

