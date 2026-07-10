# SPEC-19 — Observability, Reliability, Backup, and Support

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: all runtime specifications

## 1. Purpose

This specification defines structured logs, metrics, traces, health, SLOs, alerting, capacity, degraded modes, backups, restore, disaster recovery, integrity scrubbing, support bundles, maintenance, and operational ownership.

## 2. Reliability principles

- Durable state before acknowledgment.
- Idempotent retry and explicit external commit points.
- Cached/read-only usefulness during dependency outage.
- No false success under ambiguity.
- Backups are unproven until restored.
- Content integrity is continuously verifiable by hash.
- Operations expose correlation and recovery state.
- Local work survives UI/network failures.

## 3. Service level indicators

Hub:

- API availability by valid request class;
- API read/mutation latency;
- art-head accept transaction success;
- lock acquire/renew latency/success;
- event delivery lag;
- object upload/download success/integrity;
- build/release queue wait and outcome;
- outbox/webhook backlog;
- backup freshness/restore verification.

Desktop/plugin:

- daemon start/connect;
- plugin discovery/pair/connect;
- heartbeat/reconnect;
- capture/apply success/duration;
- build duration/success;
- UI responsiveness/crash-free sessions;
- local CAS integrity/disk pressure.

## 4. SLO targets

Initial team-mode monthly targets, excluding documented external-provider outage but not NeuMan failure:

- Hub authenticated read availability: 99.9%.
- Hub mutation availability: 99.9%.
- Lock acquire/renew p95: <500 ms within primary region.
- Event durable-to-delivered p95: <2 seconds, p99 <10 seconds.
- Object metadata p95: <500 ms; blob transfer throughput depends region/network and is separately measured.
- Audit/outbox durable lag p99: <30 seconds.
- Restore-point objective: PostgreSQL ≤5 minutes; accepted objects zero loss when durable write acknowledged and storage meets configured replication.
- Hub service restore-time objective: 4 hours for supported reference deployment.
- Desktop crash-free sessions: ≥99.5% beta target.
- Plugin connection recovery after daemon restart p95: <30 seconds.

SLOs are targets, not hidden guarantees; operators can configure and publish their own.

## 5. Error budgets

Each SLO has monthly budget. Exhaustion policy:

- freeze risky feature rollout;
- prioritize reliability/security fixes;
- increase review for migrations;
- publish incident/status to affected operators/users;
- external provider outages tracked separately but degraded UX still reviewed.

## 6. Structured logging

Canonical event fields:

```json
{
  "timestamp":"...",
  "level":"info",
  "component":"hub.lock-service",
  "event":"lock.acquired",
  "message":"Art cell lock acquired.",
  "correlationId":"...",
  "operationId":"op_...",
  "projectId":"prj_...",
  "aggregateType":"lock",
  "aggregateId":"lck_...",
  "actorPrincipalId":"...",
  "durationMs":12,
  "outcome":"success",
  "errorCode":null,
  "attributes":{}
}
```

Rules:

- JSON lines in files/server sinks; human console optional.
- UTC timestamps plus monotonic durations.
- Central schema/version.
- No raw secret/content per SPEC-18.
- User/project IDs may be hashed/pseudonymized in telemetry; local operational logs retain minimum needed.
- Log levels: trace/debug/info/warn/error/fatal with documented usage.
- High-cardinality content hashes/paths avoided in metrics but allowed redacted/local logs when necessary.

## 7. Redaction

Central redactor processes before every sink:

- header/query key allow/deny lists;
- token/API-key/JWT/OAuth-code/signed-URL patterns;
- configured path/user pseudonymization;
- repository/native content fields omitted;
- maximum field length/truncation with original hash optional.

Debug mode does not disable secret redaction. Redaction failure drops field/event rather than emits raw.

## 8. Log retention

Reference:

- desktop local rotating logs: 7 days/200 MiB max;
- Hub application logs: 30 days hot, operator-configured archive;
- security/audit distinct retention, default 1 year;
- build logs: tied to build retention, redacted/compressed;
- Studio runner/plugin logs: operation-scoped, imported/redacted, default 30 days.

Users can clear non-audit local logs. Audit retention follows policy/legal controls.

## 9. Metrics

Naming `neuman_<subsystem>_<metric>_<unit>`.

Core metrics:

- request counts/duration/status;
- active sessions/WebSockets/presence;
- DB pool/query/transaction conflicts;
- lock requests/conflicts/renew failures/expired;
- event/outbox/webhook queue depth/lag;
- object bytes/count/upload/download/hash failures;
- CAS hit/miss/eviction/corruption;
- build/release counts/durations/states;
- provider calls/rate limits/errors;
- Studio bridge messages/chunks/retries/reconnects;
- process crashes/restarts;
- disk/memory/CPU/file descriptors;
- backup age/restore check.

Labels are bounded enums/region/component/status; never project ID, user ID, cell ID, hash, or path in shared metrics by default.

## 10. Distributed tracing

- W3C Trace Context for HTTP/Hub/provider calls where accepted.
- Correlation ID across desktop-daemon-Hub-worker-provider-runner.
- Span attributes sanitized and bounded.
- Sample normal flows; retain errors/high-impact release traces at higher rate without secrets.
- Plugin loopback carries correlation IDs but does not need full OTel SDK.
- Trace loss never affects correctness.

## 11. Audit versus logs

Audit is durable domain evidence, not operational logs. It records who/what/when/result and is append-only/tamper-evident. Logs may be dropped/rotated. A critical action MUST commit audit transaction/outbox as specified even if log sink unavailable.

## 12. Health endpoints

### Liveness

Process event loop/responding; no dependency check that would cause restart storm.

### Readiness

- DB reachable/migrated;
- required object store reachable for configured role;
- secret/key material loaded;
- outbox lag below hard threshold;
- clock health;
- no maintenance block.

### Degraded health

Capabilities map:

- metadata read;
- content read/write;
- locks;
- events;
- builds;
- releases;
- GitHub/Roblox providers.

Desktop diagnostics uses similar module health, not one green/red bit.

## 13. Alerting

Severity:

- SEV0 security/data integrity/unauthorized production active;
- SEV1 widespread outage/data unavailability/partial release stuck;
- SEV2 degraded provider/queue/SLO burn/backup lag;
- SEV3 non-urgent defect/capacity trend.

Alerts:

- SLO fast/slow burn;
- DB/object unavailable;
- hash mismatch/corruption;
- backup/PITR lag or restore verification failed;
- audit/outbox stuck;
- lock renewal systemic failure;
- webhook auth failures spike;
- cross-tenant authorization anomaly;
- release unknown/partial beyond threshold;
- disk/cache pressure;
- signing/update anomaly.

Alerts link runbook and correlation, not secrets.

## 14. External provider outages

### GitHub unavailable

Cached/local Git work continues; PR/check/authorization proof blocked. Queue only idempotent safe metadata updates with expiry.

### Roblox unavailable

Local capture/build may continue; OAuth refresh/resource checks/publish/restart/drift proof block. Never queue blind production publish for automatic later execution without renewed preflight/approval.

### Hub unavailable

Local cached work/draft capture continues; shared locks/acceptance/approval/release stop. Existing lock displayed uncertain/expired based server time; no offline accepted head.

### Object store unavailable

Metadata reads/local cached objects continue; acceptance/build/release requiring missing/durability object blocks.

## 15. Backup scope

Hub backup includes:

- PostgreSQL base + WAL/PITR;
- object-store versioned/replicated objects;
- signing/audit key backup per security policy;
- deployment configuration/schema versions;
- GitHub App configuration references, not unnecessarily exported secrets;
- audit verification checkpoints;
- restore runbooks/infrastructure definitions.

Git repositories/Git LFS have their own provider durability; Hub project export provides portable second path where configured.

## 16. PostgreSQL backup

- daily full/base backup minimum;
- continuous WAL archiving target RPO 5 minutes;
- encryption in transit/at rest;
- separate credentials/account/bucket from primary where possible;
- retention daily 30 days, monthly 12 months reference;
- backup completion and restore testing monitored;
- schema migration backups tagged.

## 17. Object-store backup

- bucket versioning enabled;
- deletion protection/MFA/object lock optional high-security;
- cross-region/account replication for high durability;
- lifecycle does not delete accepted/release objects;
- inventory/hash scrub;
- metadata DB backup and object bytes both required.

Object store alone cannot reconstruct authorization/revision graph without metadata export.

## 18. Local backup

Desktop warns when local-only provider contains sole authoritative accepted content. Offers portable project export to user-selected destination. Cache is not marketed as backup. Keychain credentials are intentionally not in project export.

## 19. Restore procedure

1. Declare incident/fence writes.
2. Select restore time and record evidence.
3. Restore DB to isolated environment.
4. Verify migrations, row constraints, audit chain/outbox.
5. Restore/attach object store version and inventory.
6. Traverse accepted/release roots and hash-verify objects.
7. Reconcile GitHub webhooks/provider cursors without replaying mutations.
8. Rotate secrets if compromise suspected.
9. Run read-only conformance and sample build/release verification.
10. Cut over with new deployment epoch and notify clients to resync.
11. Preserve old environment for forensic retention.

No production cutover with missing accepted/release root objects unless explicit disaster declaration and affected projects notified.

## 20. Restore drills

- Quarterly automated restore to isolated environment.
- Semiannual full project/object/release verification and client reconnect.
- Annual regional disaster/tabletop.
- Record achieved RPO/RTO, missing documentation, integrity results.
- Failed drill is operational incident/action item.

## 21. Disaster scenarios

Runbooks:

- database loss/corruption;
- object bucket deletion/corruption;
- region/account loss;
- signing key loss/compromise;
- ransomware on desktop/local repo;
- bad migration/rolling deploy;
- event/outbox duplication/backlog;
- GitHub/Roblox long outage;
- unauthorized/partial production release.

## 22. Capacity planning

Track:

- projects/users/sessions;
- cells/revisions/events;
- blob size/count/growth/dedup;
- active locks/presence;
- build/release concurrency;
- API/webhook/event throughput;
- DB table/index growth/WAL;
- egress/storage cost.

Load tests at 2× forecast peak and failure at hard quotas. Storage growth alerts 30/14/7 days to capacity.

## 23. Support bundle

User initiates. Bundle preview lists every file/field category. Contents:

- version/platform/compatibility;
- redacted effective config;
- recent relevant logs/correlation;
- operation summaries/error codes;
- process/provider health;
- anonymized performance metrics;
- optional project manifest only with explicit checkbox;
- never credentials/native/source/DCC by default.

Bundle is locally generated, encrypted for support recipient if uploaded, expires by retention, and has hash/manifest.

## 24. Maintenance

Hub maintenance modes:

- read-only planned;
- full unavailable;
- migration;
- security lockdown;
- provider degraded.

Clients receive start/end/allowed actions. Planned maintenance prevents new locks/releases early enough; active production release is completed/fenced before migration.

Desktop maintenance: database migration/update with progress/backup/rollback and no simultaneous daemon ownership.

## 25. Acceptance criteria

1. Canary secrets absent from logs/metrics/traces/support bundles.
2. SLO dashboards/alerts use defined SLIs and bounded labels.
3. External provider outage tests preserve safe degraded modes.
4. Quarterly restore meets RPO/RTO and verifies every sampled/required root hash.
5. Lost response/partial release alert/runbook works.
6. Support bundle is previewable, redacted, and contains no project content by default.
7. Maintenance/migration prevents unsafe new locks/releases.

