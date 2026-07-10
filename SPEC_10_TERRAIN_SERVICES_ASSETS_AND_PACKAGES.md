# SPEC-10 — Terrain, Service State, External Assets, and Packages

Status: Draft; terrain encoding requires Phase 0 ADR  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-03, SPEC-09

## 1. Purpose

This specification covers Roblox state that does not fit ordinary independent model cells: voxel terrain, non-creatable services and their properties, external Roblox assets, Roblox packages, and source DCC artifacts.

## 2. Terrain authority

Terrain is Studio-authored under a declared art channel. It is partitioned into fixed, non-overlapping, grid-aligned tiles. Each tile is independently locked/versioned but a revision may checkpoint multiple tiles atomically only when their capture epochs are stable.

## 3. Terrain coordinate system

Manifest defines:

- origin in studs;
- tile size in studs `[x,y,z]`;
- Roblox voxel resolution, v1 fixed at 4 studs;
- layer name;
- allowed global extents.

Tile coordinate to region:

```text
min = origin + coordinate * tileSize
max = min + tileSize
```

All components must be multiples of resolution. Region boundaries use a specified half-open convention `[min,max)` in NeuMan metadata even if Roblox APIs use inclusive cell corners; adapter conversion has golden boundary tests.

Tile ID: `terrain_<layer>_<x>_<y>_<z>` using signed decimal grid coordinates.

## 4. No overlap invariant

V1 tiles have zero overlap. Every managed voxel belongs to exactly one tile. Brushes crossing tiles dirty every intersected tile. An edit requiring locks MUST hold all affected tiles before acceptance.

The plugin may not know brush bounds reliably from all Studio tools. It therefore supports:

- explicit user-selected dirty region;
- conservative configured neighborhood dirtying;
- full managed-terrain reconciliation at checkpoint.

Under-marking a changed tile is a validation defect; uncertain state becomes dirty/unknown.

## 5. Terrain encoding decision

Phase 0 compares two profiles. Alpha MUST select and ratify one or support both with explicit version.

### 5.1 `terrain-region-rbxm-v1` preferred

1. call `Terrain:CopyRegion` for exact aligned region;
2. serialize resulting `TerrainRegion` through `SerializationService`;
3. store native blob plus region metadata;
4. restore with deserialize plus `Terrain:PasteRegion` at exact corner.

Acceptance requires stable round-trip for occupancy, solid material, liquid occupancy, boundaries, and supported Studio versions.

### 5.2 `terrain-voxel-zstd-v1` fallback

1. call `ReadVoxelChannels` for `SolidMaterial`, `SolidOccupancy`, `LiquidOccupancy` in bounded subregions;
2. encode dimensions, palette, materials, occupancy in a documented binary format;
3. canonicalize traversal X-major then Y then Z, explicitly frozen;
4. compress with Zstandard fixed profile;
5. restore with `WriteVoxelChannels` or documented equivalent;
6. capture terrain material/service settings separately.

Fallback is more inspectable but must prove it preserves all needed terrain channels. Unsupported future channels force compatibility review.

## 6. Terrain snapshot metadata

- tile/layer/coordinate;
- exact stud and voxel bounds;
- resolution;
- encoding/version;
- content hash/size;
- non-air voxel count;
- material palette and counts;
- occupancy min/max/histogram optional;
- Studio/API schema/plugin version;
- capture session/user/time/epoch;
- neighboring tile hashes optional for seam validation;
- validation result.

## 7. Terrain capture consistency

1. acquire required tile locks/base revision;
2. ensure edit mode and no active terrain apply;
3. capture all requested tiles;
4. recapture lightweight signatures/epochs where possible;
5. if terrain changed during capture, reject/retry;
6. validate no tile missing in dirty region;
7. submit snapshots as one draft change set.

Because Roblox does not expose a universal terrain-change event, manual checkpoint and conservative reconciliation are required. The product MUST NOT claim unobserved continuous terrain capture.

## 8. Terrain apply

1. verify every tile blob/encoding/bounds;
2. snapshot current affected tiles for rollback;
3. begin ChangeHistory recording where supported;
4. clear/replace exact tile regions in deterministic coordinate order;
5. restore tiles;
6. verify channel signatures;
7. commit or restore rollback snapshots.

Partial terrain apply is not clean state. Seam validation checks boundary voxels and reports discontinuities but does not assume every discontinuity is an error.

## 9. Terrain diff

Available diff levels:

- tile hash changed;
- material/occupancy aggregate differences;
- voxel count by material;
- 2D height/material heatmap derivative;
- exact voxel delta when encoding supports it.

Voxel delta is review only; automatic merge of overlapping changed tiles is not supported.

## 10. Terrain locks

- Lock resource is tile ID.
- Multi-tile acquisition is atomic in Hub: acquire all or none, sorted by tile ID to avoid deadlock.
- Lease follows SPEC-16.
- Plugin warns before likely cross-boundary editing.
- Acceptance rejects changed tile without valid base/lock on protected channel.

## 11. Service state

Roblox services are non-creatable singleton objects. NeuMan captures only manifest-allowlisted properties and serializable child instances.

ServiceState resources include:

- Lighting
- SoundService
- MaterialService
- Workspace settings
- StarterGui settings
- other services only through reviewed adapters.

Each has one singleton resource ID and exclusive lock by default.

## 12. Service property registry

For every property:

- Roblox service/class and property name;
- API schema type;
- read capability;
- write capability;
- Studio-only/security tags;
- default value for schema/build;
- canonical tagged-value encoder;
- apply order/dependencies;
- validation range;
- whether change requires restart/reopen.

Properties not in registry are ignored with explicit index completeness warning or block per policy. Not-scriptable properties are never accessed through unsupported tricks.

## 13. Service children

Creatable children such as Atmosphere/Sky/effects may be native cells under service-owned slots. Their parent slot is service identity; apply stages children before reference resolution and service property finalization as adapter defines.

## 14. Service apply transaction

- Capture prior typed values and child roots.
- Validate all incoming values before first write.
- Apply in adapter-specified order.
- Read back writable properties.
- Restore previous values on failure.
- Record ChangeHistory when Studio supports these property changes.
- Unknown readback mismatch is failure/unknown, not success.

## 15. External asset dependency

Asset record:

```json
{
  "assetId": "123",
  "assetType": "mesh",
  "versionNumber": "7",
  "creator": {"type":"group","id":"456"},
  "sourceUri": "rbxassetid://123",
  "sourceContentHash": "b3-256:...",
  "importer": {"tool":"Studio 3D Importer","version":"...","settingsHash":"..."},
  "permissions": [{"universeId":"...","status":"allowed","observedAt":"..."}],
  "moderationStatus": "available",
  "referencedBy": [{"cellId":"cell_...","instanceId":"inst_...","property":"MeshId"}]
}
```

Fields unavailable from supported APIs are `unknown`, never assumed.

## 16. Asset extraction

Extract from semantic property types and explicit adapters:

- mesh/content/texture IDs;
- images/decals;
- audio;
- animations;
- fonts;
- video;
- package/source asset links;
- require-by-asset-ID in code validator, not art dependency acceptance.

Normalize only documented Roblox URI forms. Preserve original URI for audit. Deduplicate by asset identity/version semantics.

## 17. Asset versioning policy

- If runtime reference can pin a version, record/pin it.
- If Roblox resolves only current asset content, release-critical updates SHOULD use a new asset ID to preserve historical meaning.
- An overwritten mutable asset makes prior release reproducibility `externally-mutable`; UI/gate reports this.
- Source DCC content and import recipe SHOULD be retained so asset can be recreated.
- Asset availability/permission is rechecked at build and pre-publish.

## 18. Asset permissions

For every target universe:

- verify target creator/experience may use the asset using supported API or engine validation;
- distinguish owner, explicitly shared, public, Roblox-owned, unknown;
- block `unknown` for production when policy requires proof;
- do not auto-change permissions without an explicit authorized feature and scope;
- permission checks are timestamped and expire by policy, default 24 hours before release.

## 19. Moderation and availability

Build/release gates distinguish:

- available;
- pending moderation;
- moderated/unavailable;
- private/not permitted;
- deleted/archived;
- provider unavailable;
- unknown.

Provider outage does not equal asset failure but may block proof-required release.

## 20. Roblox packages

Package record:

- package asset ID;
- resolved version;
- package owner;
- PackageLink identity/properties;
- auto-update state;
- nested package dependencies;
- local modifications status;
- permissions by target;
- reference locations.

Release rules:

- auto-update disabled in assembled release unless policy explicitly allows nondeterminism;
- resolved version pinned in dependency manifest;
- local modifications either rejected or exported as a new owned cell/package workflow;
- updating package is a reviewed dependency change;
- package-contained scripts are code/security dependencies and scanned.

## 21. Source DCC artifacts

Examples: `.blend`, `.psd`, `.kra`, `.fbx`, `.gltf`, `.glb`, source audio/video.

Record:

- content hash/size/media type;
- storage provider path;
- lock holder/history;
- tool/version;
- license/provenance metadata;
- derived Roblox asset IDs and import recipe;
- preview derivatives.

NeuMan versions and locks these bytes but does not merge them semantically. DCC file opened outside NeuMan may bypass advisory UI; acceptance checks stale base/lock.

## 22. Custom materials and environment dependencies

MaterialService overrides, terrain material colors, SurfaceAppearance assets, environment maps, and related objects are included in service/asset manifests. A cell using a custom material must declare the corresponding service/asset dependency so checkout/build order is deterministic.

## 23. Validation

- terrain bounds/alignment/encoding valid;
- no overlapping tile identity;
- supported terrain channels preserved;
- service properties registered, typed, and within range;
- service child cells valid;
- external assets parse and meet availability/permission policy;
- packages pinned and auto-update policy satisfied;
- DCC provenance/license policy satisfied when configured;
- dependency graph has no unresolved node;
- build order can satisfy service/material/package dependencies.

## 24. Error codes

- `TAS_TERRAIN_UNALIGNED`
- `TAS_TERRAIN_ENCODING_UNSUPPORTED`
- `TAS_TERRAIN_CAPTURE_UNKNOWN`
- `TAS_TERRAIN_APPLY_FAILED`
- `TAS_SERVICE_PROPERTY_UNSUPPORTED`
- `TAS_SERVICE_READBACK_MISMATCH`
- `TAS_ASSET_ID_INVALID`
- `TAS_ASSET_PERMISSION_UNKNOWN`
- `TAS_ASSET_NOT_AVAILABLE`
- `TAS_PACKAGE_UNPINNED`
- `TAS_PACKAGE_LOCALLY_MODIFIED`
- `TAS_DCC_PROVENANCE_MISSING`

## 25. Acceptance criteria

1. Terrain encoding ADR selected from lossless round-trip corpus.
2. Boundary tests prove every managed voxel maps to exactly one tile.
3. Multi-tile locks are atomic and stale bases rejected.
4. Terrain failure injection restores prior tiles or enters recovery-required.
5. Service adapter registry has golden typed values and readback tests per Studio build.
6. Asset extraction matches representative place corpus without silent URI loss.
7. Permission/availability unknown cannot pass proof-required production gate.
8. Package auto-update and local modifications are detected.
9. Historical release reports externally mutable assets accurately.

## 26. References

External sources last verified: 2026-07-09.

- [Roblox Terrain](https://create.roblox.com/docs/reference/engine/classes/Terrain)
- [Roblox TerrainRegion](https://create.roblox.com/docs/reference/engine/classes/TerrainRegion)
- [Roblox Assets usage guide](https://create.roblox.com/docs/cloud/guides/usage-assets)
