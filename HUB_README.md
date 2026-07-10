# NeuMan Hub

NeuMan Hub is the optional self-hosted control plane for shared art authority,
acceptance leases, immutable object references, membership, build/release
evidence, durable events, and audit. The checked-in implementation is a
runnable Axum modular monolith backed by SQLite and a local content-addressed
store. It is intended for local development, demos, and single-node team
evaluation. The API and transaction boundaries are the reference contract for
the production PostgreSQL/S3 adapters.

The NeuMan project does not operate a hosted Hub, shared account system, or central project database. This binary is deployed and owned by the user/team. Its database, object store, identity provider, keys, backups, retention, and network endpoint remain under that operator's control. The desktop never defaults to a maintainer endpoint.

## Files

- `neuman_hub.rs` is the executable entry point.
- `hub.rs` contains configuration, API, authorization, repository, storage,
  event streaming, and tests.
- `hub_schema.sql` is the idempotent SQLite migration.
- `hub.env.example` lists every supported environment setting.

The workspace registers `neuman_hub.rs` as the `neuman-hub` binary.

## Local start

Set a unique development bearer token before starting. The token must contain
at least 16 characters and is immediately stored as a domain-separated SHA-256
digest; plaintext is never persisted or logged.

```powershell
$env:NEUMAN_HUB_BOOTSTRAP_TOKEN = '<long random development token>'
$env:NEUMAN_HUB_CURSOR_SECRET = '<at least 32 random characters>'
cargo run --bin neuman-hub
```

The bootstrap principal is a deployment operator, but project authorization is
still derived from persisted membership. Creating a project grants that
principal its initial `admin` membership. Clients authenticate with
`Authorization: Bearer …` and should send a stable, non-secret
`X-Neuman-Session-Id` so leases bind to a specific device/session.

All durable mutations require an `Idempotency-Key` containing 8–128 URL-safe
characters. The record is scoped by authenticated principal, project, and
route. Reuse with a different canonical request hash fails with
`HUB_IDEMPOTENCY_CONFLICT`.

## API surface

The JSON API is rooted at `/api/v1`.

- Health and discovery: `/health/live`, `/health/ready`, `/version`, and
  `/capabilities`.
- Identity: `/me`.
- Projects and authorization decisions: `/projects`, project resources,
  archive, and project memberships.
- Art authority: proposals, reviews, rejection, atomic proposal acceptance,
  revision state, and accepted channel head.
- Art proposals reference one JCS canonical full-state manifest in CAS. Hub
  parses it in the proposal transaction, verifies every cell ID/slot/hash/size,
  recomputes the domain state root, derives the changed resource set against the
  accepted base, and requires exact object/resource equality. Acceptance repeats
  validation; independent client hash/resource claims are never authoritative.
- Protected acceptance leases: list, single/batch acquire, renew, release, and
  administrator break.
- Immutable objects: upload negotiation, local transfer, verified completion,
  project metadata, batch status, and scoped download transfer.
- Build and release evidence: build requests/attempts, immutable release
  requests, hash-bound approvals, start/resume/rollback transitions.
- Durable event replay and authenticated WebSocket delivery.
- Tamper-evident, project-scoped audit export.
- Ephemeral presence heartbeat/listing.

The exact route declarations are centralized in `build_router` in `hub.rs`.
Structured errors have the shape:

```json
{
  "error": {
    "code": "HUB_BASE_STALE",
    "message": "Lease base must be the current accepted channel head.",
    "details": { "currentHeadRevisionId": "arev_…" }
  }
}
```

Project lookup is performed through membership before resource disclosure.
Knowing a project ID or content hash never grants access.

## Authority and transaction guarantees

SQLite mutations use an immediate write transaction. The following commit as
one unit:

1. authorization recheck;
2. idempotency lookup;
3. aggregate mutation and optimistic/CAS constraint;
4. audit-chain record;
5. durable outbox record;
6. idempotency response.

Proposal acceptance compares both the caller's expected head and proposal base
with the current channel head, verifies every referenced object is project
authorized and integrity-verified, verifies unexpired proposal-author leases
for changed resources, inserts the immutable revision, and advances the head by
compare-and-swap.

Batch lease acquisition sorts and de-duplicates resources, validates the
accepted base, checks every conflict and the project quota, then inserts all
leases. It inserts none on any conflict. Leases expire exactly at
`expiresAtMs`; there is no authorization grace period. Duration is 120 seconds,
the client renewal target is 30 seconds, and renewals require the holder
principal, holder session, unexpired lease, and exact renewal counter.

Events are committed in the outbox before being sent. WebSocket delivery is
at-least-once; consumers must de-duplicate by event ID and reconcile aggregates.
Signed cursors detect modification and REST replay survives disconnects.

## Object transfer

Object identity is `b3-256:<lowercase-base32>` over exact bytes. SHA-256 is
recorded as the public/LFS checksum. Negotiation validates project permission,
quota, size, media type, and hash. The upload URL contains only an opaque upload
ID; the short-lived transfer bearer is returned separately and sent in
`X-Neuman-Transfer-Token`, preventing secrets in URL logs.

The service verifies size and BLAKE3 before staging, then verifies BLAKE3 and
SHA-256 again at completion before atomically adding object metadata and the
project authorization reference. Download uses a separately scoped short-lived
bearer and verifies the physical bytes before returning them. A globally
present but project-unauthorized hash is never claimable by possession.

The local layout is:

```text
<object-dir>/tmp/<upload-id>
<object-dir>/cas/<first-2>/<next-2>/<base32-digest>
```

## Roles

- `viewer`: project metadata, objects, events, audit, and presence reads.
- `artist`: viewer access plus art proposals and acceptance leases.
- `developer`: viewer access plus art proposals, leases, and build evidence.
- `approver`: viewer access plus art reviews/acceptance and release approvals.
- `release_manager`: viewer access plus release request/execution transitions.
- `admin`: all project actions and membership/lease administration.

Roles are loaded from the database for every protected operation. Actor IDs in
request bodies are never trusted. The API has no Roblox API-key or arbitrary
code-execution surface.

## Production PostgreSQL adapter

Implement the `Repository` boundary and retain all transaction invariants.
Production tables correspond directly to `hub_schema.sql`, with these changes:

- UUID/UUIDv7-capable columns and JSONB instead of opaque SQLite text where
  operationally useful;
- database server time for lease, approval, and retention decisions;
- `SELECT … FOR UPDATE` on accepted channel heads and release aggregates;
- deterministic row/advisory locking for all-or-none lease batches;
- tenant/project row-level security as defense in depth;
- transactional outbox workers using `FOR UPDATE SKIP LOCKED`;
- partition/retention strategy for outbox and audit tables;
- migration jobs run before compatible rolling API deployment;
- continuous WAL archiving and tested point-in-time recovery with RPO ≤5 min.

Use separate least-privilege roles for migration, API, workers, backup, and
read-only operations. API transactions must never broadcast before commit.

## Production S3 adapter

Replace local transfer I/O with an object provider while retaining the Hub DB
as authorization authority. Physical keys are
`objects/v1/b3/<first-2>/<next-2>/<digest>`. Required controls:

- public access blocked and TLS required;
- bucket versioning and server-side encryption (operator KMS where required);
- exact method/object/size scoped, short-lived signed transfers;
- multipart part/count/expiry bounds;
- full BLAKE3/SHA-256 verification by a trusted streaming/finalizer path—S3
  ETag is never treated as SHA-256;
- lifecycle rules restricted to temporary/quarantine objects;
- accepted and release roots protected from physical GC;
- inventory/hash scrubbing and cross-account/region replication for the
  high-security profile.

Physical blob existence and project authorization reference remain distinct.
Never create a project reference from client metadata alone.

## Security and operations

Development mode may use loopback HTTP. Non-development configuration rejects
non-HTTPS public URLs, short cursor secrets, and development bootstrap tokens.
Terminate TLS at a trusted reverse proxy, restrict CORS at that proxy, set
request/body/connection rate limits, and keep the Hub private until real
OIDC/JWKS authentication and production adapters are configured.

Operational logs are structured JSON with request correlation. The service
does not log bearer tokens, transfer tokens, native bytes, signed URLs, or
environment contents. The database stores only development token/transfer
digests. Use an external secret manager for production OIDC, database, object,
and audit-signing credentials.

Readiness verifies schema migration and object-store availability without
disclosing provider secrets. Liveness deliberately has no dependency checks.
Back up both relational metadata and exact object bytes; neither alone can
restore the revision/authorization graph. Quarterly restores must validate the
audit chain and traverse/hash every accepted and release root before cutover.

SQLite is a single-process development profile. Do not place it on a shared
filesystem, run multiple writers, or claim the production SLOs with it.

## Verification

Run:

```powershell
cargo test --bin neuman-hub
cargo clippy --bin neuman-hub --all-targets -- -D warnings
```

Inline tests cover:

- hashed bearer authentication;
- cross-project non-disclosure;
- idempotent replay and conflicting key reuse;
- all-or-none lease batches and renewal replay;
- accepted-head compare-and-swap;
- object hash verification, transfer, and authorization;
- signed event cursor tampering.

Before a production adapter is accepted, add fault-injection tests for database
deadlocks/retries, object completion ambiguity, outbox duplication, point-in-
time restore, clock health, rolling migrations, and multi-tenant property tests.
