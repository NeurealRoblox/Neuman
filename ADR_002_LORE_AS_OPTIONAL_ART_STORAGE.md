# ADR-002: Lore is an optional art-storage adapter, not the v1 authority

Status: Accepted  
Decision date: 2026-07-10  
Review gate: `P0-09` storage benchmark and Lore 1.0/maturity reassessment

## Context

Epic Games' Lore is an MIT-licensed, binary-first version control and storage system. Its architecture is centralized, content-addressed, Merkle/revision based, chunked, deduplicated, and capable of sparse/on-demand hydration. Lore also exposes its storage subsystem independently of version control, which is a close conceptual fit for NeuMan's immutable native-cell objects.

However, NeuMan has two additional constraints:

- users must be able to run the public desktop with no NeuMan-operated database or service; and
- RBXM/RBXL content needs Roblox-aware ownership, references, dirty-cell protection, Studio preview, and explicit acceptance. A byte-oriented VCS cannot provide those semantics by itself.

Lore is currently a 0.x project. Its published roadmap says scalable enforced locking is still in progress; basic locking currently informs rather than enforces at the intended scale. Its open-source desktop and web collaboration clients are also roadmap work. These are important for artist-facing protected native content.

## Decision

NeuMan v1 keeps its authority model independent of any storage provider:

- Git commit is code authority.
- Accepted NeuMan art revision plus cell hashes is native-art authority.
- The local content-addressed store is the mandatory offline baseline.
- The optional self-hosted Hub coordinates leases, proposals, accepted-head compare-and-swap, and fanout.
- Provider adapters may store immutable cell bytes in local CAS, Git LFS, S3-compatible storage, or Lore without changing revision IDs, state roots, policy, or receipts.

Lore is therefore a promising optional adapter and benchmark candidate, not the mandatory v1 VCS and not a replacement for NeuMan's semantic art ledger.

## Proposed Lore adapter boundary

The first supported experiment SHOULD use Lore's standalone storage API rather than replacing the Git code lane or exposing Lore revisions as NeuMan art authority:

1. Ingest an already-verified RBXM cell blob into a project/tenant-scoped Lore partition.
2. Persist the returned Lore address only as provider-location metadata beside NeuMan's canonical content hash.
3. Fetch into a bounded temporary file, then independently verify size and canonical NeuMan hash before CAS admission.
4. Never treat possession of a Lore address as authorization.
5. Keep accepted-head compare-and-swap, leases, review, and audit in the user-owned local ledger or optional self-hosted Hub.
6. Support full export back to plain files plus a provider-independent NeuMan manifest.

Using the Lore VCS subsystem for an all-in-one repository MAY be revisited later, but it must not force teams off GitHub for code/review or require a vendor-hosted service.

## Why this is the conservative world-class choice

- Lore's content-defined chunking may substantially reduce transfer and storage for small edits to large binary cells, so it deserves real corpus measurements.
- NeuMan cells are already content-addressed and immutable, so the adapter is narrow and reversible.
- Lore cannot create a correct semantic merge for two Roblox scenes; its own design documentation says binary merge requires format-specific tooling and recommends locks for unmergeable content.
- Making a new 0.x server mandatory would weaken the local-first/no-central-service promise and enlarge the security, backup, migration, and operations surface before benefits are measured.
- Provider independence lets Lore become the best backend if it wins, without coupling durable project history to its current implementation.

## Promotion criteria

`P0-09` must publish a reproducible representative corpus comparing local CAS, raw Git, Git LFS, S3-compatible CAS, and Lore for:

- initial import and small/large edit transfer bytes;
- cold/warm checkout and sparse hydration latency;
- disk amplification and cross-revision deduplication;
- corruption detection and interrupted upload/download recovery;
- lock correctness, lease integration, and concurrent push behavior;
- self-host deployment, authentication, tenant isolation, backup/restore, and upgrade cost;
- Windows/macOS client packaging and API stability;
- complete provider-independent export and re-import.

Lore may become a recommended adapter only if the benchmark, threat model, restore drill, and compatibility record pass. It may become a default only through a replacement ADR with a migration and rollback plan.

## Primary references

- [Lore overview and architecture](https://lore.org/)
- [Lore system design](https://epicgames.github.io/lore/explanation/system-design/)
- [Lore roadmap](https://epicgames.github.io/lore/roadmap/)
