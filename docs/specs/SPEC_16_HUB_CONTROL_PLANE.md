# SPEC-16 — Hub Control Plane

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02–04, SPEC-09

## 1. Purpose

NeuMan Hub is the optional self-hosted team control plane for project membership, accepted art revisions, locks, presence, event relay, approvals, shared build/release metadata, GitHub webhooks, audit, and object-store coordination.

Hub is not required for local-only editing/builds. Protected shared channels, enforced acceptance locks, centralized approvals, and real-time remote presence require Hub or a conforming implementation.

The NeuMan open-source project does not operate a multi-tenant Hub, central Hub database, or official hosted endpoint. Every Hub deployment is owned and administered by the user, team, or an explicitly selected third party outside the official product trust boundary. The desktop never auto-discovers, defaults to, or silently enrolls in a maintainer-operated endpoint.

## 2. Architecture

V1 reference is a modular Rust/Axum service with:

- PostgreSQL authoritative relational state;
- S3-compatible immutable blob store;
- transactional outbox for durable events;
- WebSocket event gateway;
- background workers in same binary or separate worker role;
- pluggable OIDC/GitHub authentication;
- GitHub App webhook endpoint;
- OpenTelemetry observability.

Start as a modular monolith. Service separation is not required until measured scale/operational need.

## 3. Trust and tenancy

- Deployment is multi-project and MAY be multi-organization.
- Every row/resource carries tenant/project identity as applicable.
- Authorization filters run before lookup result disclosure to avoid ID enumeration.
- Object possession/hash does not grant access.
- Hub does not accept Roblox API keys from desktop users.
- Hub never executes art, repository code, or arbitrary Studio runner code.

## 4. API conventions

Base: `/api/v1`. HTTPS required except explicit local development.

- JSON content type.
- Bearer authentication.
- request ID accepted/generated.
- idempotency key required for mutations creating durable effects.
- optimistic concurrency with `If-Match`/aggregate version where relevant.
- cursor pagination: `limit` default 50, max 200, opaque signed cursor.
- timestamps/IDs/errors per SPEC-00/02.
- response includes `ETag` for cacheable resources.
- unknown major version is 404/compatibility response, never guessed.

## 5. Core endpoints

### Health/version

```text
GET  /health/live
GET  /health/ready
GET  /api/v1/version
GET  /api/v1/capabilities
```

Readiness checks DB migrations, object-store basic access, outbox lag threshold, and required secret configuration. It does not expose secret/provider details.

### Sessions/accounts

```text
GET    /api/v1/me
GET    /api/v1/sessions
DELETE /api/v1/sessions/{sessionId}
POST   /api/v1/accounts/link/{provider}
DELETE /api/v1/accounts/{provider}/{accountId}
```

### Projects/memberships

```text
GET    /api/v1/projects
POST   /api/v1/projects
GET    /api/v1/projects/{projectId}
PATCH  /api/v1/projects/{projectId}
POST   /api/v1/projects/{projectId}:archive
GET    /api/v1/projects/{projectId}/members
POST   /api/v1/projects/{projectId}/members
PATCH  /api/v1/projects/{projectId}/members/{principalId}
DELETE /api/v1/projects/{projectId}/members/{principalId}
```

### Art channels/revisions/proposals

```text
GET  /api/v1/projects/{projectId}/art-channels
GET  /api/v1/projects/{projectId}/art-channels/{channelId}
GET  /api/v1/projects/{projectId}/art-channels/{channelId}/head
GET  /api/v1/projects/{projectId}/art-revisions/{revisionId}
GET  /api/v1/projects/{projectId}/art-revisions/{revisionId}/state
POST /api/v1/projects/{projectId}/art-proposals
GET  /api/v1/projects/{projectId}/art-proposals/{proposalId}
POST /api/v1/projects/{projectId}/art-proposals/{proposalId}/reviews
POST /api/v1/projects/{projectId}/art-proposals/{proposalId}:accept
POST /api/v1/projects/{projectId}/art-proposals/{proposalId}:reject
```

### Locks

```text
GET    /api/v1/projects/{projectId}/locks
POST   /api/v1/projects/{projectId}/locks:acquire
POST   /api/v1/projects/{projectId}/locks:acquireBatch
POST   /api/v1/projects/{projectId}/locks/{lockId}:renew
DELETE /api/v1/projects/{projectId}/locks/{lockId}
POST   /api/v1/projects/{projectId}/locks/{lockId}:break
```

### Presence

```text
POST /api/v1/projects/{projectId}/presence:heartbeat
GET  /api/v1/projects/{projectId}/presence
```

### Builds/releases

Hub may coordinate/record, while builders/publishers remain workers/clients:

```text
POST /api/v1/projects/{projectId}/builds
GET  /api/v1/projects/{projectId}/builds/{buildId}
POST /api/v1/projects/{projectId}/builds/{buildId}/attempts
POST /api/v1/projects/{projectId}/builds/{buildId}:cancel
POST /api/v1/projects/{projectId}/releases
GET  /api/v1/projects/{projectId}/releases/{releaseId}
POST /api/v1/projects/{projectId}/releases/{releaseId}/approvals
POST /api/v1/projects/{projectId}/releases/{releaseId}:start
POST /api/v1/projects/{projectId}/releases/{releaseId}:resume
POST /api/v1/projects/{projectId}/releases/{releaseId}:rollback
```

### Objects/uploads

```text
POST /api/v1/projects/{projectId}/objects:negotiateUpload
POST /api/v1/projects/{projectId}/uploads/{uploadId}:complete
GET  /api/v1/projects/{projectId}/objects/{contentHash}
GET  /api/v1/projects/{projectId}/objects/{contentHash}:download
POST /api/v1/projects/{projectId}/objects:batchStat
```

Direct object URLs are short-lived, project/operation-scoped, method/size/content-hash constrained where storage supports it.

### Audit/events

```text
GET /api/v1/projects/{projectId}/audit-events
GET /api/v1/projects/{projectId}/events
GET /api/v1/events/stream   (WebSocket upgrade)
```

## 6. Database model

Core tables:

- `principals`
- `external_accounts`
- `sessions`
- `organizations` optional
- `projects`
- `project_memberships`
- `project_policy_revisions`
- `art_channels`
- `art_revisions`
- `art_revision_parents`
- `art_revision_changes`
- `art_state_entries` or compact state manifests
- `art_proposals`
- `reviews`
- `locks`
- `lock_history`
- `presence_sessions`
- `builds`, `build_attempts`
- `release_bundles`
- `releases`, `release_place_steps`, `approvals`, `waivers`
- `deployments`, `drift_observations`
- `objects`, `object_references`, `uploads`
- `audit_events`
- `outbox_events`
- `webhook_deliveries`
- `idempotency_records`

Primary IDs are opaque UUID-based strings. Foreign keys, unique constraints, check constraints, and row-version columns enforce invariants in addition to application logic.

## 7. Transaction boundaries

Critical transactions:

### Accept art proposal

Atomically:

1. lock channel head row;
2. verify expected head/base;
3. verify proposal status/reviews/policy/locks/object presence;
4. persist accepted revision/state references;
5. compare-and-swap channel head;
6. consume relevant approval/lock evidence as policy defines;
7. write audit event;
8. write outbox event;
9. commit.

No event is broadcast before commit.

### Release approval/start

Approval insertion and policy evaluation are transactional. Start locks immutable request version and creates durable place-step plan/outbox event atomically.

### Object reference

Object upload completion verifies hash/size then atomically marks available and creates project authorization reference. A blob existing globally does not create project access until reference transaction commits.

## 8. Lock semantics

### 8.1 Lease defaults

- duration: 120 seconds;
- renewal cadence client target: 30 seconds;
- server grace for network jitter: none for authorization after `expiresAt`; UI may display short reconnect grace but acceptance uses exact expiry;
- max duration without renewal: lease duration;
- max continuous hold: operator policy, default 8 hours before explicit re-acquire/review.

### 8.2 Acquire request

- resources sorted and unique;
- channel;
- base accepted revision;
- branch/workstream identifier;
- holder session/principal;
- intended action/cell current hash.

Grant conditions:

- permission;
- no conflicting active lease;
- base revision is current accepted head or policy-approved descendant relationship;
- every previous lock result affecting resource is incorporated into requested base;
- resource exists or creation lock allowed;
- quota/maintenance state allows.

Batch acquire is all-or-none in one DB transaction using deterministic row/advisory lock order.

### 8.3 Branch-aware behavior

Default protected channel permits one global resource lock only when requester base includes current head. Experimental parallel branch locks are disabled until merge semantics and UI explicitly support them.

### 8.4 Renewal

Requires lock ID, holder session, renewal counter, current base/work state, and unexpired lease. Counter increments atomically. Holder change requires release/new lock.

### 8.5 Expiry/release/break

- expiry worker marks/history and broadcasts; DB queries treat `expiresAt <= now` as inactive even before worker.
- normal release authenticated by holder/project admin and records outcome proposal/draft if supplied.
- break requires project-admin, reason, recent auth, audit; it does not make stale local edits acceptable.
- reconnect does not resurrect expired lock; acquire new and rebase.

### 8.6 Enforcement scope

Hub enforces accepted-head mutations and conflict detection. It cannot physically prevent every Roblox Studio tool/Team Create write. UI language says “protected acceptance lock,” not guaranteed editor filesystem lock.

## 9. Presence

Ephemeral data:

- principal/session/device display;
- active project/place/channel;
- selected cell(s) optional;
- mode editing/reviewing/building;
- lock IDs;
- last heartbeat;
- privacy setting.

Presence TTL default 45 seconds; heartbeat 15 seconds. Stored in memory/Redis optional, not long-term audit except meaningful lock/session events. Users may hide selection but not lock ownership.

## 10. Event stream

WebSocket subscribes to authorized project topics. Envelope uses domain events plus stream cursor.

Client supplies last cursor; Hub replays from durable event store/outbox retention. If cursor expired, sends `resync-required` and client fetches current state.

Event categories:

- project/membership/policy;
- art proposal/revision/head;
- lock/presence;
- object/upload;
- build/release/deployment/drift;
- GitHub/check integration;
- system maintenance/security.

At-least-once delivery. Clients deduplicate by event ID and reconcile aggregates.

## 11. Idempotency

Mutation request requires `Idempotency-Key` for create/accept/start/publish coordination. Hub stores principal/project/route/request canonical hash/result/status for at least 24 hours, longer for release actions.

Same key/different request is `HUB_IDEMPOTENCY_CONFLICT`. In-progress duplicate returns operation reference.

## 12. Background workers

- outbox publisher;
- expired lock/presence cleanup;
- object upload finalizer/verification;
- garbage-collection mark preparation;
- GitHub webhook processor/reconciler;
- build/release coordinator;
- notification dispatcher;
- retention/pseudonymization;
- integrity scanner;
- backup verification scheduler.

Workers use leases/idempotency and can be horizontally replicated.

## 13. Object-store behavior

Per SPEC-17. Hub DB owns authorization/reference metadata; object store holds immutable bytes. Hash is verified server-side or by trusted completion flow. Client-provided metadata alone never marks object valid.

## 14. Quotas

Per deployment/project defaults operator-defined:

- members;
- active locks;
- concurrent uploads/builds;
- object bytes and object count;
- event/webhook rate;
- API requests;
- release concurrency;
- retention.

Quota response includes limit/current/reset/remediation and does not partially accept an atomic request.

## 15. Rate limiting

Separate buckets:

- unauthenticated auth/discovery;
- authenticated reads;
- mutations;
- expensive state/diff queries;
- upload negotiation;
- WebSocket connects/events;
- webhook ingress by App/source.

Keys combine deployment, principal/session, project, IP as appropriate. Do not let one tenant exhaust all workers; global circuit breakers plus fair queues.

## 16. Audit

Append-only logical audit stream records all security/high-impact/project authority changes. Audit event has hash chain or signed batch anchoring to make tampering evident. DB admins can still control infrastructure; tamper-evidence is not a claim of immutable hardware trust.

Audit export supports canonical JSON and verification tool.

## 17. Data retention

- accepted revisions/releases/deployments: project retention, default indefinite;
- rejected proposals/drafts: default 90 days metadata, unreferenced blobs GC later;
- presence: seconds/minutes only;
- auth/security/audit: operator/legal policy, default 1 year;
- webhook raw bodies: minimum needed, default 7 days encrypted/redacted;
- logs/traces: SPEC-19.

Project deletion is staged: archive, retention/legal checks, export, tombstone, object ref release, delayed GC.

## 18. Deployment

Reference modes:

- single binary + PostgreSQL + S3 for small team;
- multiple stateless API/worker replicas behind TLS load balancer;
- sticky session not required if WebSocket supported by gateway and event backend;
- migrations run as explicit job before new replicas;
- rolling deploy only across compatible API/schema versions.

Config through environment/secret manager; no secrets in image or Git.

## 19. Failure handling

- DB unavailable: readiness false, no mutation; cached health only.
- Object store unavailable: metadata reads may continue, content operations/builds block.
- Outbox lag: mutation may continue below threshold; readiness/degraded status and event clients reconcile.
- WebSocket loss: REST state remains; reconnect/cursor replay.
- Worker crash: leases/idempotency recover.
- Clock drift: NTP health alert; DB/server time authoritative for leases/approvals.
- Split brain prevented by DB constraints/leases; caches never authoritative.

## 20. Security

- TLS, secure headers, strict CORS allowlist; desktop native clients do not need broad browser CORS.
- OIDC issuer/audience/JWKS validation.
- Per-resource authorization.
- SQL parameterization, schema validation, request limits.
- SSRF-safe GitHub/provider clients.
- Signed short-lived object URLs.
- Secret manager and key rotation.
- No arbitrary code execution/build in API process.
- Tenant isolation tests and audit.
- Admin endpoints separate/strongly authenticated.

## 21. Error codes

- `HUB_UNAVAILABLE`
- `HUB_PROJECT_NOT_FOUND`
- `HUB_PERMISSION_DENIED`
- `HUB_VERSION_CONFLICT`
- `HUB_IDEMPOTENCY_REQUIRED`
- `HUB_IDEMPOTENCY_CONFLICT`
- `HUB_QUOTA_EXCEEDED`
- `HUB_LOCK_CONFLICT`
- `HUB_LOCK_EXPIRED`
- `HUB_BASE_STALE`
- `HUB_OBJECT_MISSING`
- `HUB_OBJECT_UNAUTHORIZED`
- `HUB_CURSOR_EXPIRED`
- `HUB_MAINTENANCE`

## 22. Acceptance criteria

1. Tenant/project authorization cannot be bypassed by guessed IDs/hashes.
2. Art accept transaction atomically advances head/audit/outbox or none.
3. Batch locks are all-or-none; expiry and stale base tests pass under concurrency.
4. Event stream converges after duplicates, disconnect, cursor replay, and resync.
5. Idempotency prevents duplicate proposal/release effects.
6. API/worker rolling upgrade and DB migration compatibility tested.
7. Object-store outage never marks missing bytes available.
8. Backup restore produces valid state/object references/audit verification.
