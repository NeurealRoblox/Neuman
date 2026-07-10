# SPEC-07 — Roblox Studio Plugin

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02–04, SPEC-08–10

## 1. Purpose

The NeuMan Studio plugin is the only supported live bridge between an editable Roblox DataModel and the local NeuMan daemon. It captures Studio-owned art, terrain, and selected service state; applies accepted revisions without reopening Studio; displays locks/conflicts; and participates in engine-side validation.

It does not own Git, cloud credentials, builds, releases, or project administration.

## 2. Supported execution context

- Roblox Studio edit mode on Windows and macOS.
- Installed local plugin or Creator Store plugin with a known signed release identity.
- Compatible Studio build and plugin/bridge protocol.
- HTTP requests enabled when Roblox requires it for loopback requests.
- `SerializationService`, `HttpService:CreateWebStreamClient`, `ChangeHistoryService`, `StudioService`, `Selection`, `RunService`, `Terrain`, and other documented plugin APIs as capability-tested.

If a required capability is absent, plugin enters read-only incompatible state and provides exact remediation.

## 3. Packaging and modules

Reference Luau modules:

- `Bootstrap`
- `Compatibility`
- `BridgeClient`
- `Pairing`
- `SessionContext`
- `ProjectBinding`
- `OwnershipMap`
- `CellRegistry`
- `ChangeTracker`
- `NativeSerializer`
- `TerrainAdapter`
- `ServiceStateAdapter`
- `ReferenceResolver`
- `ApplyTransaction`
- `LockClient`
- `Validation`
- `WidgetApp`
- `SettingsStore`
- `Diagnostics`

Network, Studio mutation, and UI modules MUST remain separated so protocol parsing can be tested without DataModel access and mutations can be tested with fixtures.

## 4. Installation and update

Supported channels:

- Creator Store release for public stable/beta;
- daemon-installed local plugin for development and pinned enterprise deployments.

The plugin displays version/channel. It MUST NOT self-download or execute code. Desktop may install a signed `.rbxm` plugin package only with explicit user action and checksum verification.

Update behavior:

1. `Unloading` stops new captures/applies.
2. Active apply transaction finishes or aborts safely.
3. Lock leases remain daemon-owned; plugin disconnect does not release them automatically unless configured.
4. Settings and uncommitted local draft markers persist.
5. New plugin performs compatibility handshake before mutations.

## 5. Widget and toolbar

Toolbar:

- Open NeuMan
- Checkpoint Selected Cell
- Apply Incoming Changes
- Connection status indicator

Dock widget sections:

1. Connection/pairing
2. Active project/place/channel
3. Selection and owning cell
4. Cell status/lock
5. Incoming changes/conflicts
6. Checkpoint/apply actions
7. Diagnostics

Critical status is visible without opening the widget through toolbar badge/notification where Studio APIs permit.

## 6. Startup lifecycle

```text
loading -> compatibility-check -> disconnected
disconnected -> discovering -> pairing-required | connecting
pairing-required -> pairing -> connected
connecting -> connected | disconnected
connected -> bound | unbound | incompatible
bound <-> degraded
bound -> disconnecting -> disconnected
```

No DataModel mutation occurs before `bound` and exact project/place ownership validation.

## 7. Compatibility check

At load, plugin records:

- plugin semantic version and build hash;
- supported bridge protocol range;
- Studio version/build/channel when accessible;
- availability of required classes/methods/enums;
- API schema fingerprint supplied by daemon versus plugin embedded expectations;
- buffer/base64/Zstandard/WebSocket behavior probes that do not mutate the place.

Unknown Studio version is warning for capture and blocking for production runner behavior unless policy has an approved compatibility record.

## 8. Pairing and session binding

Pairing follows SPEC-08. Plugin settings may retain:

- daemon installation ID;
- opaque renewable pairing credential;
- last discovery port;
- non-secret project/workspace preference;
- plugin instance ID.

Plugin settings MUST NOT contain OAuth/GitHub/Hub tokens, object-store credentials, API keys, or release approvals.

On connection, plugin sends:

- plugin instance/version/protocol;
- Studio user ID from documented Studio API;
- universe ID, place ID, game ID as available;
- edit/play state;
- place session fingerprint;
- current DataModel name;
- capability report.

Daemon returns a signed/bound session context containing project/workspace/place/channel and allowed actions. Plugin verifies every mutation command matches this context.

## 9. Place identity validation

Before capture/apply:

- reported universe/place matches configured authoring or sandbox target;
- Studio mode is allowed;
- project marker, if present, matches project ID;
- authoring channel is explicit;
- Studio user mismatch policy is evaluated;
- ownership roots exist or initialization is explicitly requested;
- no duplicate cell IDs are present in managed scope.

Saving a local copy may yield no cloud IDs. Local files require a daemon-issued local place session binding and cannot publish without later target verification.

## 10. Ownership map

Daemon sends resolved ownership entries with configuration hash. Plugin builds a path/instance lookup cache and invalidates it on relevant reparent/name changes.

Rules:

- Plugin refuses to register a cell under Git-owned/generated/external-package roots.
- Plugin does not capture Git-owned descendants.
- Incoming apply cannot create a child that crosses into another ownership root.
- Unknown instances follow root policy: preserve, warn, or reject. `delete` is not a default unknown policy.

## 11. Cell identity

Cell root contains attribute:

```text
NeuManCellId = "cell_<uuidv4>"
```

Optional attributes:

- `NeuManProjectId`
- `NeuManRootId`

Only cell ID is required in the authoring place. Duplicate or malformed ID blocks checkpoint. Creating ID:

1. verify selected instance is eligible/creatable root;
2. acquire registration intent from daemon when online;
3. generate UUIDv4 through `HttpService:GenerateGUID(false)`;
4. set attribute within ChangeHistory recording;
5. register with daemon;
6. roll back attribute on failed registration when safe.

Production builds MAY strip these attributes per manifest; authoring and developer places SHOULD retain them.

## 12. Cross-cell instance identity

Instances participating in cross-cell references receive:

```text
NeuManInstanceId = "inst_<uuidv4>"
```

The plugin SHOULD assign this only to cell roots and instances that are a source/target of an external reference, not every descendant. Duplicate IDs block checkpoint. Within-cell references rely on native serialization.

## 13. Change tracking

Signals used where supported:

- `DescendantAdded`
- `DescendantRemoving`
- `AncestryChanged`
- `Changed`
- `AttributeChanged`
- collection tag signals as configured
- `ChangeHistoryService` recording events
- play/edit state changes

Per cell, tracker maintains:

- monotonic local mutation epoch;
- last event time;
- dirty reason set;
- last known accepted snapshot/state;
- last checkpoint draft;
- capture in progress flag;
- lock state.

Signals are hints, not proof of equality. Final checkpoint always serializes/validates authoritative content.

Event storms are debounced at 250 ms default for UI state. Automatic native serialization is never performed for every drag frame. Manual checkpoint is default; idle draft capture is opt-in and never auto-accepts protected-channel changes.

## 14. Consistent cell capture

For each cell:

1. validate identity/ownership/lock/base state;
2. record mutation epoch `e0`;
3. wait for Studio to exit an active plugin apply and configured edit stabilization window;
4. call `SerializationService:SerializeInstancesAsync({cellRoot})`;
5. record mutation epoch `e1`;
6. if `e0 != e1`, discard bytes and retry up to configured limit;
7. extract external reference table and dependency hints;
8. send metadata and native bytes using SPEC-08;
9. daemon hashes/indexes/validates;
10. plugin marks draft checkpoint only after daemon receipt.

Multi-cell checkpoint captures each cell plus an epoch vector. If any captured cell changes before the transaction closes, daemon treats the set as a draft with mixed epoch and requires retry; it cannot become an accepted atomic art revision.

## 15. Native serialization

- Use `SerializationService` only, not a handwritten complete `.rbxm` encoder in plugin.
- Root MUST be creatable on deserialize.
- Buffer converts to transfer chunks without lossy string conversion.
- Serializer errors include cell ID/class/path, not raw content.
- A capability fixture suite determines supported classes for current Studio build.
- Native bytes are never modified by plugin after serialization.

No stability guarantee is assumed. Studio build/API schema is part of snapshot metadata.

## 16. Checkpoint validation in plugin

Fast preflight:

- forbidden scripts/classes;
- duplicate cell/instance IDs;
- ownership escape;
- missing required attributes;
- invalid or unresolved supported external refs;
- excessive size estimate after serialization;
- active play mode policy;
- package auto-update policy hints;
- lock/base mismatch.

Daemon/build validators remain authoritative and may find more issues.

## 17. Locks

Plugin displays lock holder, base revision, expiry, connection health, and renewal state.

Lock semantics:

- lock required before edit when policy says so;
- plugin can warn and visually mark a non-held cell, but Roblox Studio does not provide a universal reliable write barrier for all tools/Team Create edits;
- Hub enforcement occurs at checkpoint acceptance: only valid holder/base may update protected head;
- detecting changes to a cell held by another principal creates `lock-violation` status; plugin MUST NOT silently revert Team Create edits;
- expired lease makes cell unprotected and blocks acceptance until reacquired/rebased.

## 18. Incoming revision preflight

Before any mutation:

- session/project/place/channel/config hashes match;
- target revision is accepted or explicitly allowed draft;
- all required blobs received and hashes verified by daemon;
- plugin/Studio/schema compatibility passes;
- local cell dirty state known;
- affected cells not locally dirty unless conflict resolution command supplied;
- incoming cell IDs unique and ownership slots valid;
- enough memory/time budget available;
- play mode policy allows apply;
- ChangeHistoryService can begin recording.

Preflight failure makes zero DataModel changes.

## 19. Apply transaction

### 19.1 Staging

For every incoming model cell:

1. receive daemon-verified buffer;
2. deserialize to detached instances;
3. require exactly expected root count/type/CellId;
4. validate subtree forbidden content and identity;
5. store in detached temporary table, not DataModel.

Terrain/service changes stage their typed operations separately.

### 19.2 Commit

1. begin one `ChangeHistoryService:TryBeginRecording` for bounded transaction;
2. snapshot references/parents/order and retain old roots in memory;
3. detach old affected roots;
4. parent all new roots to declared slots in deterministic order;
5. apply service properties and terrain operations in specified order;
6. resolve external references after all roots exist;
7. verify required roots/IDs/references;
8. destroy/dereference replaced old roots only after verification;
9. finish recording with Commit;
10. send apply receipt to daemon.

If any step before commit verification fails, restore old roots/properties/terrain where possible and finish recording Cancel. If rollback cannot prove restoration, mark session `recovery-required`, preserve both old/new content in a conflict container where safe, and block further mutation.

Large revisions MAY be split into multiple declared transactions only if release/art state is not marked fully applied until all succeed. Partial apply is explicit.

## 20. Applied-state verification

Raw reserialization may not be byte-deterministic. Verification levels:

- `exact-native` — reserialized bytes match when compatibility profile proves deterministic;
- `semantic-equivalent` — canonical semantic/reference/dependency fingerprint matches;
- `applied-unverified` — structural checks pass but opaque equality unavailable;
- `failed`.

Project policy decides allowed level. Production build assembly requires the level specified by toolchain compatibility record.

After known apply, baseline is trusted until a Studio change signal or session reopen. On reopen, plugin recaptures fingerprints; ambiguity is dirty/unknown, never clean.

## 21. Local conflicts

A conflict preserves:

- base accepted snapshot;
- local current capture/draft;
- incoming snapshot;
- cell identity/path;
- validation and reference information.

Actions:

- keep local and create proposal;
- discard local then apply incoming;
- duplicate local as new cell ID then apply incoming;
- open comparison workflow;
- cancel.

No action is automatic for same-cell divergence.

## 22. Terrain and service state

Terrain and service behavior follows SPEC-10. Plugin MUST not treat `Workspace.Terrain` or other services as ordinary creatable model roots. Terrain capture is explicit per configured tile; service properties use allowlisted typed adapters.

## 23. Play/test behavior

Default:

- capture disabled during play/test;
- structural applies queued until edit mode;
- connection remains for status;
- code/art updates do not mutate the running simulation;
- stopping test triggers context revalidation before queued apply.

Future live-safe apply profiles require separate class/property allowlists and are not part of v1.

## 24. Multiple Studio sessions

Each has unique session ID. Daemon routes by exact session. UI never broadcasts mutation to all sessions without per-session preflight. Two sessions for same personal place are distinct local states.

Plugin `SetSetting` writes are minimized because multiple Studio instances may race. Session-critical state lives in daemon and DataModel markers; settings use versioned compare/readback where possible.

## 25. Performance and limits

Defaults:

- UI event coalesce: 100 ms;
- dirty debounce: 250 ms;
- max simultaneous serializations: 1 per Studio session;
- warning cell size: 20 MiB;
- hard cell policy default: 96 MiB;
- transfer raw chunk: 256 KiB;
- max pending inbound memory: 16 MiB before spill/flow control; plugin cannot spill to disk, so daemon throttles;
- maximum six total WebStream clients is respected; plugin uses one.

Plugin yields during large scans and reports progress. It must not freeze Studio for more than 100 ms continuous work target outside unavoidable engine serialization calls.

## 26. Security

- Connect only to numeric loopback addresses.
- Reject redirect/endpoint hostnames, non-loopback IPs, and unexpected ports outside discovery result.
- Verify session token/message sequencing/project binding.
- Never execute received Luau.
- Art apply rejects scripts by default.
- UI sanitizes all remote text.
- Pairing credential grants only local plugin protocol, not cloud API access.
- Debug logs omit native payloads and credentials.

## 27. Error codes

- `STU_INCOMPATIBLE`
- `STU_NOT_PAIRED`
- `STU_WRONG_PLACE`
- `STU_UNBOUND_PROJECT`
- `STU_OWNERSHIP_VIOLATION`
- `STU_DUPLICATE_CELL_ID`
- `STU_CELL_DIRTY`
- `STU_LOCK_REQUIRED`
- `STU_CAPTURE_UNSTABLE`
- `STU_SERIALIZE_FAILED`
- `STU_DESERIALIZE_FAILED`
- `STU_CHANGE_RECORDING_UNAVAILABLE`
- `STU_APPLY_ROLLED_BACK`
- `STU_RECOVERY_REQUIRED`
- `STU_PLAY_MODE_DEFERRED`

## 28. Acceptance criteria

1. Golden place corpus round-trips supported classes across compatibility matrix.
2. Change storms do not produce partial accepted captures.
3. Dirty same-cell incoming update never overwrites.
4. Apply is undoable as one logical action for bounded revisions.
5. Injected failures at every apply step restore prior state or enter explicit recovery-required state.
6. Wrong place/project/channel blocks all mutations.
7. Plugin contains no cloud credentials or arbitrary code execution path.
8. Multi-session routing and reconnect are deterministic.
9. Studio remains responsive within performance budget on representative places.

## 29. References

External sources last verified: 2026-07-09.

- [Roblox SerializationService](https://create.roblox.com/docs/reference/engine/classes/SerializationService)
- [Roblox HttpService streaming](https://create.roblox.com/docs/reference/engine/classes/HttpService)
- [Roblox Studio plugins](https://create.roblox.com/docs/studio/plugins)
