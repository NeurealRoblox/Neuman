# SPEC-09 — Art Cells, Revisions, Merge, and Diff

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-03

## 1. Purpose

This specification defines how Roblox-native art is partitioned, identified, captured, stored, versioned, reviewed, merged, checked out, and validated. It intentionally avoids pretending arbitrary Roblox binary content is safely property-mergeable.

## 2. Authority model

- The configured Studio authoring channel is the only place where art edits originate.
- A captured draft is evidence, not a build input.
- An accepted ArtRevision is the immutable build authority.
- Native cell bytes are reconstructive authority.
- Semantic indexes, previews, and change summaries are review aids.
- Production capture can propose adoption but cannot directly advance the accepted authoring head.

## 3. Cell boundaries

A cell is the smallest independently versioned and locked art unit. Eligible roots are creatable instances that `SerializationService` can round-trip in the current compatibility profile, normally `Model` or `Folder`.

Good cells:

- map zone with internal constraints/references;
- building or prop set;
- rig and its attachments;
- VFX group;
- UI screen hierarchy;
- reusable scene assembly.

Bad cells:

- entire complex place by default;
- a single part when thousands of cross-cell constraints result;
- mixed art and authoritative scripts;
- service objects such as `Lighting` or `Terrain`;
- package content that should remain external and pinned.

## 4. Cell registration

Required root attribute: `NeuManCellId`. Registration record includes project, place key, ownership root, art channel, root class, initial path/name, creator, and creation time.

Constraints:

- unique within project art state;
- UUID never reused after deletion;
- root may move within allowed ownership root without changing ID;
- moving across ownership root requires explicit migration proposal;
- copying a cell duplicates its attribute in Studio, so duplicate detection requires assigning a new ID before checkpoint;
- nested cells are forbidden in v1 unless parent cell policy declares child cell slots and serialization excludes nested roots; default is no nesting.

## 5. Native snapshot object

Required objects:

```text
nativeBlob       exact `.rbxm` bytes
metadata         canonical JSON
semanticIndex    canonical JSON
references       canonical JSON
dependencies     canonical JSON
previews         optional image/video blobs
validation       canonical JSON
```

### 5.1 Metadata schema

```json
{
  "schemaVersion": "1.0",
  "cellId": "cell_...",
  "rootClass": "Model",
  "displayName": "Harbor West",
  "logicalParent": "/Workspace/Art",
  "native": {
    "contentHash": "b3-256:...",
    "sha256": "sha256:...",
    "sizeBytes": 12345,
    "mediaType": "application/x-roblox-rbxm"
  },
  "capture": {
    "studioBuild": "...",
    "apiSchemaHash": "b3-256:...",
    "pluginVersion": "0.1.0",
    "sessionId": "ses_...",
    "universeId": "111",
    "placeId": "222",
    "capturedAt": "...",
    "capturedBy": "...",
    "mutationEpoch": 42
  },
  "bounds": {},
  "counts": {},
  "semanticIndexHash": "b3-256:...",
  "referenceTableHash": "b3-256:...",
  "dependencyManifestHash": "b3-256:...",
  "validationHash": "b3-256:..."
}
```

Native hash covers exact bytes. Metadata hash excludes provider location and mutable access URLs.

## 6. Stable instance references

Within-cell references are preserved by native serialization. Cross-cell references use `NeuManInstanceId` on participating instances.

Reference table entry:

```json
{
  "source": {"cellId":"cell_a","instanceId":"inst_x","property":"Attachment1"},
  "target": {"cellId":"cell_b","instanceId":"inst_y"},
  "required": true,
  "expectedClass": "Attachment"
}
```

Extraction uses the compatibility API schema to identify `Ref`-typed readable properties and explicit adapters. If a reference crosses the cell and cannot be represented/read, checkpoint fails or cell boundaries must change. Path-only external references are forbidden.

## 7. Semantic index

Purpose: review, diff, validation, search, and conservative equivalence. It is not sufficient to rebuild a cell.

Index header:

- schema/API hash;
- cell/root identity;
- native hash;
- instance count;
- unsupported/opaque field summary;
- completeness level.

Per instance:

- stable local index ID;
- NeuMan instance ID if present;
- class/name;
- parent local ID;
- deterministic sibling ordinal for duplicate names;
- tags and attributes except configured generated metadata;
- readable serialized properties with tagged types;
- bounding/pivot information where applicable;
- referenced Roblox assets/packages;
- source-code presence flag, but source omitted for forbidden art scripts;
- opaque/unreadable property names.

Local index ID is derived within one snapshot from deterministic traversal and is not durable across moves. Matching across snapshots uses:

1. NeuManInstanceId;
2. unique cell-relative structural identity;
3. class/name/parent/fingerprint heuristic marked confidence;
4. otherwise add/remove, not guessed rename.

## 8. Typed property canonicalization

- Integers preserve exact value/type.
- Floats preserve IEEE-754 bit representation for hashing; UI also renders decimal.
- `CFrame` stores position plus 3×3 rotation float bits.
- Colors store component float bits and display color space metadata.
- Enums store enum type and symbolic item plus numeric value when available.
- Content/asset references store normalized URI and extracted ID/version.
- Tags sorted Unicode codepoint order.
- Attributes sorted key order with tagged values.
- Ref values use local or external identity objects.
- Shared/binary strings store hash/size, not raw bytes in index.
- Unknown values store typed opaque marker and MUST reduce completeness.

Canonicalization version is explicit and independently versioned.

## 9. Completeness levels

- `complete-supported` — all properties in compatibility profile represented.
- `supported-with-opaque` — native preserved; one or more unreadable/unknown properties.
- `structure-only` — only hierarchy/basic identity available.
- `index-failed` — native exists but no reliable index.

Project policy controls acceptance. `index-failed` never passes normal protected channel acceptance.

## 10. Bounds and counts

Metadata SHOULD include:

- axis-aligned bounding box and pivot for spatial content;
- part/model/mesh/constraint/attachment/UI/particle counts;
- triangle/mesh complexity only when provider exposes reliable data;
- external asset/package/reference counts;
- native and preview size.

Unavailable metrics are omitted, not fabricated.

## 11. Art state representation

An ArtState is a sorted map:

```text
cell/<CellId>              -> CellSnapshot content hash
terrain/<TerrainTileId>    -> Terrain snapshot content hash
service/<ServiceStateId>   -> Service snapshot content hash
dependency/<scope>         -> Dependency manifest hash
```

Keys are UTF-8 bytes in ascending lexicographic order. Deleted resources are absent from current state and appear as changes/tombstones in revision history.

## 12. Art state root algorithm v1

Digest bytes are raw BLAKE3-256 outputs, not encoded strings.

For each entry:

```text
leaf = BLAKE3(
  "neuman-art-leaf-v1\0" ||
  U32BE(len(keyBytes)) || keyBytes ||
  U32BE(len(valueHashStringBytes)) || valueHashStringBytes
)
```

Leaves remain in key order. Tree construction:

1. Empty state root is `BLAKE3("neuman-art-empty-v1\0")`.
2. A single leaf root is that leaf digest.
3. Pair adjacent nodes left/right and hash:

```text
node = BLAKE3("neuman-art-node-v1\0" || leftDigest || rightDigest)
```

4. If a level has an odd final node, promote it unchanged to the next level; do not duplicate it.
5. Repeat until one root.

Persisted string is `b3-256:` plus lowercase base32-no-padding root digest.

Golden vectors MUST freeze this algorithm before Alpha.

## 13. Revision change set

Each change:

- resource key;
- kind `add | update | delete`;
- old hash or absent;
- new hash or absent;
- optional semantic summary hash;
- originating capture/session/lock evidence.

Applying changes to first-parent complete state MUST reproduce state root. Revision hash/signature covers parents, state root, canonical change set, author, message, policy/validation evidence.

## 14. Channels and heads

An art channel has one accepted head pointer updated with compare-and-swap:

```text
expectedHead == currentHead
AND proposal.parent includes expectedHead
AND policy/lock/approval valid
```

Failure is a stale-head conflict. A protected head cannot be force-updated through normal API. Administrative repair is separate, audited, and retains prior pointer history.

## 15. Capture and proposal

1. Plugin creates stable native snapshots.
2. Daemon verifies hashes and produces indexes.
3. Draft ArtRevision references complete resolved state.
4. Validators run.
5. User supplies message and submits proposal.
6. Hub/local policy records base head and lock evidence.
7. Review/approval occurs.
8. Acceptance atomically persists revision and advances head.

Draft snapshot uploads may deduplicate. Failed proposal does not delete blobs immediately.

## 16. Three-way merge

Inputs: base state `B`, ours `O`, theirs `T`.

For every union key:

1. if `O == T`, result that value;
2. else if `O == B`, result `T`;
3. else if `T == B`, result `O`;
4. else conflict.

This applies to add/update/delete equally, with absence as a value. There is no automatic native property merge in v1.

Conflict record:

- resource key/type;
- base/ours/theirs hashes;
- semantic diffs where available;
- dependency/reference impacts;
- resolution state and actor.

Resolved merge creates a new snapshot/value when manually reconciled; selecting ours/theirs records choice.

## 17. Rename and move

Cell rename/reparent within allowed root is an update to the same CellId. Semantic diff reports rename/move. Moving across ownership root/channel requires explicit migration:

- validate destination owner;
- update registration metadata;
- preserve history/CellId when destination remains Studio art;
- otherwise export/import with new authority and audit.

## 18. Deletion

Deletion proposal requires:

- cell absent from capture or explicit delete action;
- lock/base validity;
- reverse external-reference analysis;
- preview of dependent breakage;
- confirmation for high-fanout cells.

Accepted deletion removes state key and retains tombstone in revision event. Applying deletion detaches/destroys root within undo recording only after reference policy passes.

## 19. Checkout/apply plan

Compute delta between local known art state and target state. Plan operations:

- add cells;
- update cells;
- delete cells;
- terrain/service changes;
- external-reference second phase.

Order:

1. validate all content;
2. add/update roots in dependency-safe groups;
3. resolve refs;
4. apply deletions with reverse ref checks;
5. service/terrain ordering per SPEC-10;
6. verify final state/equivalence.

Local dirty resources block their operations while independent clean resources MAY apply if UI/policy explicitly allows partial apply. Workspace target state remains partial until complete.

## 20. Diff classifications

- cell added/deleted/changed;
- cell renamed/moved;
- instance added/deleted/moved/renamed/class changed;
- property changed;
- attribute/tag changed;
- dependency added/removed/version/permission changed;
- reference added/removed/retargeted/unresolved;
- opaque content changed/unknown;
- validation regression/improvement;
- size/complexity change.

Diff confidence: `exact-id`, `structural-high`, `heuristic`, `unknown`. UI must show confidence for inferred matches.

## 21. Preview generation

Previews are optional derivatives:

- thumbnail PNG/WebP;
- fixed-camera turntable/video;
- bounding-box wireframe;
- UI screenshot;
- semantic tree snapshot.

Preview generator version/settings/hash are recorded. Preview failure does not invalidate native content unless policy requires preview review.

## 22. Validation rules

Required baseline:

- native hash/size valid;
- root count/type/CellId valid;
- no duplicate cell/instance IDs;
- no nested cell violation;
- no forbidden scripts/classes;
- no ownership escape;
- external references represented/resolved;
- dependencies extractable and permitted per policy;
- size within limits;
- Studio/schema compatibility acceptable;
- index completeness meets policy;
- no NaN/infinite transform values where engine does not safely support them;
- no cyclic cross-cell apply dependency that resolver cannot handle.

## 23. Serialization migration

When Studio compatibility changes:

1. retain original native blob forever while referenced;
2. open/deserialize through an approved Studio migration runner;
3. serialize new representation;
4. compare semantic index/reference/dependencies;
5. require review for any semantic/opaque difference;
6. create migration snapshot linked to original via provenance;
7. never rewrite historical ArtRevision; a migration revision updates state.

## 24. Storage limits and guidance

- recommended cell native size 1–20 MiB;
- warning above configured threshold;
- hard default 96 MiB;
- place aggregate preflight respects Roblox place limit and publication method;
- high external-reference count warning default 100;
- high descendant count warning operator-configured from performance testing.

## 25. Error codes

- `ART_CELL_ID_INVALID`
- `ART_CELL_ID_DUPLICATE`
- `ART_NESTED_CELL`
- `ART_NATIVE_CORRUPT`
- `ART_INDEX_INCOMPLETE`
- `ART_REFERENCE_UNRESOLVED`
- `ART_DEPENDENCY_INVALID`
- `ART_SIZE_LIMIT`
- `ART_BASE_STALE`
- `ART_HEAD_CONFLICT`
- `ART_BINARY_CONFLICT`
- `ART_MERGE_UNRESOLVED`
- `ART_MIGRATION_REVIEW_REQUIRED`

## 26. Acceptance criteria

1. State-root golden vectors match Rust, TypeScript, and independent test implementation.
2. Three-way merge property tests cover every presence/change combination.
3. Cell copy/rename/reparent/delete identity tests pass.
4. Cross-cell references survive multi-cell apply or block safely.
5. Unsupported/opaque data is never silently dropped by accepted migration.
6. Raw native bytes can be retrieved for every referenced accepted revision.
7. Semantic index explicitly reports completeness/confidence.
8. Stale head/lock proposals cannot advance protected channel.

