# SPEC-17 — Storage, CAS, Git LFS, S3, and Lore Providers

Status: Draft; Lore provider experimental  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-09, SPEC-14

## 1. Purpose

This specification defines immutable content storage, local cache, provider interface, integrity, authorization, layouts, uploads/downloads, Git LFS mapping, S3/Hub behavior, optional Lore integration, replication, garbage collection, retention, encryption, export, and recovery.

## 2. Storage principles

- Content address derives from exact bytes.
- Objects are immutable.
- Metadata/pointers are mutable only through versioned transactions.
- Possession of hash is not authorization.
- Hash verification occurs at every untrusted boundary.
- Storage deduplication is not semantic merge.
- Accepted history and release roots prevent GC.
- Provider portability is a product requirement.

## 3. Object identity

Primary: `b3-256:<base32>` exact bytes.  
Published checksum: `sha256:<hex>` exact bytes.

Object metadata:

- primary/secondary hashes;
- size;
- media type;
- representation/schema version;
- created/verified timestamps;
- integrity status;
- compression/encryption-at-rest metadata external to bytes;
- provider locations;
- project authorization references;
- retention/refcount roots;
- producer/provenance.

Same bytes/media representations deduplicate even across projects only if deployment policy permits physical dedup; authorization remains separate.

## 4. Media types

Required registry includes:

- `application/x-roblox-rbxm`
- `application/x-roblox-rbxl`
- `application/xml` for Roblox XML artifacts
- `application/vnd.neuman.art-metadata+json`
- `application/vnd.neuman.semantic-index+json`
- `application/vnd.neuman.terrain+octet-stream`
- `application/vnd.neuman.release-bundle+json`
- `application/vnd.neuman.provenance+json`
- image/video preview types
- generic DCC MIME plus extension metadata.

Clients do not trust MIME alone; magic/structure validators run where safe.

## 5. Local CAS layout

```text
cas/v1/b3/<first2>/<next2>/<full-base32-digest>
cas/v1/tmp/<uuid>
cas/v1/quarantine/<digest>-<timestamp>
```

Object file is exact bytes with no custom header. Metadata is SQLite/provider DB so file remains portable. Sidecar is optional diagnostic cache, not authority.

## 6. Atomic local put

1. create exclusive temp file in same volume;
2. stream bytes while computing BLAKE3/SHA-256/size;
3. enforce maximum size/quota;
4. flush/fsync according to durability class;
5. compare expected hash/size if supplied;
6. create parent directories safely;
7. atomic rename/link to digest path without overwriting different bytes;
8. insert/update metadata transaction;
9. if existing object, verify size and scheduled full hash policy then discard temp.

Crash leaves temp object reclaimed after quarantine age.

## 7. Read/verification

Read by content hash plus authorized project context. Local cache verifies:

- file exists/regular/non-symlink;
- size;
- full hash on first untrusted acquisition, corruption suspicion, periodic scrub, or release-critical use;
- optional sampled verification for warm reads, never substitute at release-critical boundary.

Hash mismatch moves file to quarantine, invalidates provider-presence cache, and refetches from another trusted provider if possible.

## 8. Provider interface

Normative capabilities:

```text
capabilities() -> ProviderCapabilities
stat(project, hashes[]) -> ObjectStatus[]
put(project, source, expectedHash, metadata, idempotencyKey) -> PutReceipt
get(project, hash, byteRange?) -> verified stream
deletePhysical(hash, gcAuthorization) -> DeleteReceipt
listProjectReferences(project, cursor) -> refs
exportProject(project, destination) -> ExportReceipt
health() -> ProviderHealth
```

Optional:

- multipart/resume;
- signed URLs;
- server-side copy/replication;
- object versioning;
- lock service (not used as NeuMan art lock unless conformance proven);
- lazy hydration;
- range reads.

Provider `deletePhysical` is unavailable to ordinary clients. Project unreference and physical GC are distinct.

## 9. Upload negotiation

1. client `batchStat` hashes/sizes;
2. provider responds present/authorized, present/not-authorized, missing, forbidden;
3. for missing, create upload session with exact expected hash/size/media type/project;
4. stream/multipart upload;
5. provider verifies complete hash/size;
6. create project reference transaction;
7. return immutable receipt.

Client cannot claim existing global object into a project without authorization policy/transaction.

## 10. Download

- authorization before URL/stream;
- short-lived URL scoped to GET, object, range/size where possible;
- no secrets in filename/query logs;
- client verifies full expected hash before use;
- resume ranges verify final full hash;
- content-disposition uses safe generated filename.

## 11. Encryption

- TLS in transit.
- Local CAS relies on OS disk encryption by default; optional application encryption is future ADR because it affects dedup/key management.
- S3 encryption at rest enabled (SSE-S3 or operator KMS); bucket denies plaintext transport/public access.
- Database metadata encryption for sensitive fields.
- Signed URLs short-lived.

Content hashes reveal equality/size metadata; threat model and tenant policy disclose this. Cross-tenant physical dedup MAY be disabled.

## 12. Git LFS provider

### 12.1 Use

Default GitHub-native storage for `.rbxm`, terrain blobs, DCC sources, and large previews in smaller/medium projects.

### 12.2 Mapping

Git LFS pointer uses SHA-256 OID/size. NeuMan metadata maps BLAKE3 primary hash to LFS SHA-256. Deterministic repository path:

```text
.neuman/blobs/<media-category>/<b3-base32>.<safe-extension>
```

Pointer is committed. Native bytes live in LFS. Reading a pointer as native content is a typed error.

### 12.3 Attributes

`.gitattributes` patterns explicitly track blob paths with `filter=lfs diff=lfs merge=lfs -text`. Changes are security/trust relevant. NeuMan validates LFS is active before staging.

### 12.4 Upload/commit

1. place verified bytes at deterministic path through safe materialization;
2. ensure LFS clean filter produces pointer with matching SHA-256/size;
3. stage only intended pointer/art metadata paths;
4. commit/PR per SPEC-11/12;
5. verify LFS object uploaded before marking shared art revision available;
6. Hub/proposal references commit and content hashes.

### 12.5 Limits/cost

Provider reports plan/file/storage/bandwidth limits where APIs permit. NeuMan warns before large upload but GitHub remains authoritative. Project budget can block optional previews/DCC uploads, never silently omit release-required native blobs.

### 12.6 Locks

Git LFS lock API, if provider supports it, MAY mirror/display but is not authoritative art acceptance lock unless implementation passes NeuMan lock conformance. Hub locks remain default.

## 13. S3 provider

Object key:

```text
objects/v1/b3/<first2>/<next2>/<digest>
```

No project ID in physical key when cross-project dedup enabled; authorization references in DB. If isolation policy disables dedup, prefix by tenant/project and document export mapping.

Required bucket configuration:

- public access blocked;
- versioning recommended;
- server-side encryption;
- lifecycle only for explicitly tagged temp/quarantine, not accepted objects;
- access logs/CloudTrail equivalent per operator policy;
- CORS only exact trusted clients if direct upload; native desktop signed URL does not require broad origins.

Multipart upload has bounded parts, checksum, expiry cleanup. Completion verifies full content hash through server-side worker or trusted streaming path; ETag is not assumed SHA-256.

## 14. Hub provider

Hub provider combines S3 bytes with Hub authorization/ref metadata and negotiated upload/download. It is recommended team default. Desktop caches locally and may use Git metadata pointers without duplicating every blob in Git LFS when configured.

## 15. Local-only provider

For solo/offline:

- local CAS is authoritative;
- project export/backup warning prominent;
- no claim of team durability;
- accepted revision metadata stored in Git/project ledger where configured;
- publication blocked if required artifact exists only on volatile cache and policy requires backup.

## 16. Lore provider (experimental)

### 16.1 Rationale

Lore offers binary-first chunked storage, sparse hydration, version APIs, and future locks. Current pre-1.0 APIs/protocols and advisory lock limitations prevent v1 dependency.

### 16.2 Adapter rules

- Exact pinned Lore version/checksum/server compatibility.
- Store NeuMan objects as opaque immutable files under deterministic hash paths or use Lore storage API only through documented public API.
- NeuMan content hash remains identity; Lore revision/hash is provider receipt.
- NeuMan ArtRevision/locks remain domain authority unless a future conformance ADR replaces them.
- Provider export must recover all exact bytes without Lore.
- API/format change requires migration/compatibility test.
- Mark UI/project experimental and not default.

### 16.3 Benchmark gate

Representative corpus:

- transform one part;
- rename/reparent;
- modify CSG/mesh/surface;
- large map cell small edit;
- terrain tile edit;
- DCC binary edit.

Measure initial size, delta upload/download, dedup ratio, CPU/memory, clone/hydration, branch/lock behavior, corruption recovery, operational complexity versus Git LFS/S3. No marketing claim before published reproducible results.

### 16.4 Promotion to stable

Requires:

- Lore stable compatibility policy acceptable;
- enforced scalable locks or explicit continued NeuMan locks;
- Windows/macOS/Hub SDK tests;
- backup/export/restore drill;
- six-month dogfood without unrecoverable data;
- security review;
- ADR/spec status change.

## 17. Replication/mirrors

Project may configure one write authority and read mirrors. Write receipt is successful only when authority durable. Asynchronous mirrors record lag/last verified.

Release policy MAY require at least two durable copies. Mirror conflict is impossible for exact hash; same hash different bytes is catastrophic hash/integrity incident.

Provider failover for writes is manual/controlled: verify authoritative reference state, fence prior writer, then switch manifest/provider epoch. Split-brain pointer writes prohibited.

## 18. References and retention roots

Roots:

- accepted ArtRevision states;
- retained proposals/drafts;
- successful builds/release bundles;
- releases/deployments/rollback targets;
- audit/legal holds;
- explicit project pins;
- active uploads/operations.

Object reference includes project, aggregate, purpose, created/expiry, retention class. Refcount is cache; GC derives mark set from authoritative references.

## 19. Garbage collection

1. snapshot authoritative references at GC epoch;
2. mark reachable hashes recursively through manifests;
3. compare objects not marked and older than quarantine minimum 7 days;
4. write deletion candidate report;
5. optional dry run/project notifications;
6. delete provider objects with GC authorization/idempotency;
7. retain tombstone/audit receipt;
8. scrub dangling references and alert—never auto-delete reference.

Concurrent new reference after mark uses epoch/write barrier or excludes candidate. Accepted/release roots never rely only on mutable refcount.

## 20. Integrity scrubbing

- periodic metadata/size checks;
- sample/full hashes by risk/age;
- release-critical objects full verify before release;
- provider version/cross-mirror comparison;
- corruption quarantine/refetch;
- alert and block if no healthy copy;
- signed scrub receipt.

## 21. Export/import

Portable export contains:

- project metadata/art revisions/build/release manifests;
- every referenced object exact bytes;
- canonical index mapping hash/media/size;
- checksums/signatures;
- no credentials or signed URLs;
- optional encrypted archive profile with separately supplied key.

Import verifies everything before creating project references, handles ID collisions explicitly, and never overwrites existing different project state silently.

## 22. Quotas and disk pressure

- Soft/hard limits per project/provider.
- Upload negotiation rejects over hard limit before bytes where possible.
- Local cache evicts least-recently-used unreferenced/refetchable objects.
- Never evict sole authoritative local object without export/backup confirmation.
- Reserve disk threshold stops materialization with remediation.

## 23. Error codes

- `STO_HASH_INVALID`
- `STO_HASH_MISMATCH`
- `STO_OBJECT_MISSING`
- `STO_OBJECT_UNAUTHORIZED`
- `STO_OBJECT_CORRUPT`
- `STO_QUOTA_EXCEEDED`
- `STO_UPLOAD_EXPIRED`
- `STO_PROVIDER_UNAVAILABLE`
- `STO_LFS_NOT_INSTALLED`
- `STO_LFS_POINTER_UNMATERIALIZED`
- `STO_LFS_UPLOAD_MISSING`
- `STO_S3_COMPLETION_UNVERIFIED`
- `STO_LORE_INCOMPATIBLE`
- `STO_GC_REFERENCE_CONFLICT`
- `STO_EXPORT_INCOMPLETE`

## 24. Acceptance criteria

1. Exact bytes round-trip every provider and portable export.
2. Hash corruption detected at all untrusted boundaries.
3. Project authorization cannot be gained from hash knowledge/presigned URL reuse.
4. Git LFS pointer/native confusion tests fail safely.
5. GC concurrency never deletes newly/recurrently referenced object.
6. Sole local authoritative object cannot be evicted.
7. S3 multipart/ambiguous completion reconciles without false availability.
8. Lore remains optional and export proves no lock-in.
9. Full restore verifies every accepted/release object.

## 25. References

External sources last verified: 2026-07-09.

- [GitHub Git LFS](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-git-large-file-storage)
- [Epic Lore repository](https://github.com/EpicGames/lore)
- [Lore FAQ](https://epicgames.github.io/lore/faq/)
