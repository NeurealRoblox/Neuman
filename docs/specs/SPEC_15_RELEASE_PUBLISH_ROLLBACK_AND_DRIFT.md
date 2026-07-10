# SPEC-15 — Release, Publish, Rollback, and Drift

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-13, SPEC-14

## 1. Purpose

This specification defines release composition, environment gates, approvals, staging proof, ordered multi-place publication, commit points, verification, server restart, rollback/compensation, interruption recovery, drift detection/adoption, and audit.

## 2. Principles

- A release promotes an immutable ReleaseBundle.
- Production is never built from branch/channel heads at publish time.
- Every target and predecessor is revalidated immediately before mutation.
- Multi-place release is a saga, not a transaction.
- Partial success is a first-class state.
- A lost response after mutation is unknown until reconciled, never automatically retried.
- Rollback is another controlled release operation with evidence.

## 3. Environment definition

Environment has:

- key/display name;
- kind;
- production-impact boolean;
- target mappings;
- required predecessor environment/proof;
- approval policy;
- publication method policy;
- drift policy;
- verification profile;
- restart policy;
- concurrency/order policy;
- rollback policy;
- retention/audit policy.

Security uses `productionImpact`, not name matching.

## 4. Release request

Immutable request content:

- project/environment;
- bundle/logical build;
- selected place set;
- exact Roblox targets;
- publication method per place;
- publication order/dependency graph;
- expected current deployment/predecessor;
- staged proof reference;
- verification/restart plan;
- rollback targets;
- release notes hash;
- policy snapshot hash.

Changing any immutable field creates new request version and invalidates approvals.

## 5. Place dependency graph

Manifest declares edges such as `entry depends on gameplay`. Topological ordering is deterministic. Recommended:

1. backward-compatible shared/back-end places;
2. leaf/destination places;
3. lobby/entry place last;
4. server restart after all place publishes unless policy requires phased restart.

Cycles are invalid unless project supplies explicit compatible batch plan; Roblox still publishes individually.

## 6. Gates

Baseline release gates:

- bundle signature/integrity;
- build success and non-expired required evidence;
- code commit policy/CI proof;
- accepted art revision;
- dependency/asset permission availability for exact target;
- toolchain compatibility;
- no unresolved ownership/reference/validation issue;
- required staging proof for same bundle;
- target identity/creator/permission;
- drift status allowed;
- rollback target available;
- required approvals valid;
- no conflicting active release/lock;
- publication method compatible.

Gate output includes ID, result, evidence, checked time, expiry, remediation, waiver eligibility.

## 7. Waivers

Only explicitly waivable gates can be waived. Waiver includes:

- gate/release/target scope;
- authorized actor and recent auth;
- reason/ticket;
- expiry;
- risk acknowledgment;
- optional second approval.

Integrity, authentication, target mismatch, corrupt bundle, and missing publication authorization are never waivable.

## 8. Approval

Approval binds exact request hash and policy. It records actor, role, auth evidence, time, decision, comment, and audit signature.

Rules:

- distinct-principal requirement honored;
- requester/approver separation if configured;
- approval expires default 24 hours or sooner on evidence expiry;
- target/bundle/order/method/policy change invalidates;
- revoked/suspended approver invalidates unconsumed approval where policy requires;
- approval authorizes release, not provider permissions.

## 9. Staging proof

Proof contains:

- same bundle/logical build hash;
- staging target/version per place;
- publication receipts;
- verification test results;
- server/log observation window;
- completion time and expiry;
- known differences from production target (configuration only, never code/art input);

Production policy defines acceptable age, default 24 hours. A rebuilt bundle requires new proof.

## 10. Preflight

Immediately before each place publish:

1. verify release still active/approved;
2. verify bundle objects/signatures local;
3. fetch/observe exact target metadata;
4. verify actor/provider permission;
5. observe current deployment/version/drift;
6. compare expected predecessor;
7. recheck expiring asset/permission evidence;
8. confirm publication method compatibility;
9. acquire per-target release lease;
10. emit preflight receipt.

Predecessor mismatch stops saga before that place. Already published places cause partial/rollback decision.

## 11. Commit point

Per publication method:

- Open Cloud: request accepted and provider creates/returns version; lost response requires version reconciliation.
- Studio-assisted: `SavePlaceAsync` call begins potential external mutation; after invocation cancellation cannot guarantee stop.
- Manual: user confirms Publish action; commit observed only when new version/evidence appears.

UI distinguishes safe-to-cancel, cancellation-requested, and cannot-cancel/external-commit-unknown.

## 12. Per-place publish algorithm

1. `pending`
2. `preflighting`
3. `ready`
4. `publishing`
5. `reconciling` if response ambiguous
6. `published-unverified`
7. `verifying`
8. `verified` or `verification-failed`

Terminal failures include `not-published`, `unknown`, or `published-failed-verification`; these have different rollback behavior.

## 13. Publication concurrency

Default serial according to dependency order. Parallel publication is allowed only for graph-independent places and project policy; each target has independent lease/predecessor. Entry place remains last by default.

Concurrent releases to same project/environment/target are mutually exclusive. Staging may allow separate isolated targets.

## 14. Verification

After publish:

- observe version number/metadata;
- compare release marker/build identity where possible;
- launch or identify staging/canary server through supported flow;
- execute approved smoke tests;
- inspect server status/logs when supported;
- verify teleport/topology contracts configured;
- observe error/performance window;
- record confidence and evidence.

Production verification MUST NOT run unsafe automated player behavior or undocumented interaction automation.

## 15. Server restart

Policy modes:

- none;
- prompt after publish;
- automatic after full verification/approval;
- phased/canary if supported.

Before restart, show forecast/impact. Restart operation has its own receipt/state. Publish can succeed while restart fails; release status exposes this.

## 16. Release completion states

- `published` — all selected places published and required verification/restart gates pass.
- `published-with-warning` — allowed non-blocking post-publish warning.
- `partially-published` — some target commit points succeeded and others did not.
- `rollback-required` — policy mandates compensation.
- `failed-no-change` — no target commit point succeeded.
- `unknown-external-state` — at least one target could not be reconciled.

Only `published`/allowed warning is normal success.

## 17. Rollback plan

For each place:

- previous known-good bundle/logical build;
- previous Roblox version/evidence;
- rollback publication method compatible with content;
- asset/package availability and permissions;
- order/dependency implications;
- whether data/schema changes make code rollback unsafe;
- required approvals.

Rollback target is validated when release is created and again when invoked.

## 18. Rollback execution

Rollback is a new saga linked to original release:

1. identify actually published targets;
2. reconcile unknown states;
3. select previous bundle/version per policy;
4. evaluate compatibility/data warnings;
5. obtain emergency/normal approval;
6. publish compensating targets in safe order;
7. verify and optionally restart;
8. record rolled-back deployment.

Roblox version restore in dashboard/Studio is manual-handoff evidence if no API path. Restoring a saved version may still require publication to make live.

## 19. Roll-forward

If rollback unsafe or partial, create corrected bundle/release. UI presents roll-forward as separate option with explicit rationale; it cannot reuse failed mutable inputs.

## 20. Interruption and resume

Durable after every place step. On daemon/Hub restart:

- acquire/recover release lease;
- query receipts/provider/Studio evidence;
- determine per-place state;
- do not reissue publish until absence of commit is proven;
- require user decision for ambiguous manual/Studio states;
- preserve approvals only if request/evidence remains valid.

## 21. Deployment marker

Optional generated metadata in place identifies:

- project/place key;
- logical build/bundle hash;
- release ID;
- build timestamp/schema.

It contains no credential/user personal data and cannot alone prove content equality. It improves observation/drift correlation.

## 22. Drift detection

Inputs:

- expected last deployment receipt;
- observed Roblox place version/metadata;
- version history signal;
- deployment marker;
- Studio capture state root/semantic comparison.

Classification:

- clean authoritative/strong;
- version drift;
- content drift;
- unknown.

Observation age/confidence shown. A version change due save but not publish is distinguished when evidence supports it.

## 23. Drift adoption

1. Freeze expected deployment evidence.
2. Use Studio capture of configured managed roots.
3. Separate Git-owned code differences and Studio-owned art differences.
4. Reject authority violations (production-edited code cannot silently become Git main).
5. Create art proposal and/or Git branch/PR `adopt/production-<timestamp>`.
6. Run standard review/validation/build/staging.
7. Accepted changes return through a new release.

No direct channel-head/main fast-forward.

## 24. Drift overwrite

To replace unauthorized drift with known accepted build:

- inspect/capture backup first unless explicit emergency waiver;
- create new release of accepted bundle;
- target mismatch/predecessor waiver requires high-impact approval;
- preserve evidence of overwritten version;
- verify afterward.

## 25. Release notes and audit

Release record includes:

- notes/ticket;
- immutable request/approvals/gates/waivers;
- build/provenance;
- per-place receipts/versions;
- verification/restart;
- warnings/partial/rollback;
- actor/correlation/times;
- final signed receipt.

## 26. Notifications

Notify relevant roles for:

- approval requested/expiring;
- production started;
- per-place failure/unknown;
- partial publication;
- rollback required/start/result;
- drift detected;
- release completion.

Notifications contain minimal private data and link to authorized detail.

## 27. Error codes

- `REL_BUNDLE_INVALID`
- `REL_GATE_FAILED`
- `REL_APPROVAL_REQUIRED`
- `REL_APPROVAL_INVALIDATED`
- `REL_STAGING_PROOF_MISSING`
- `REL_TARGET_LEASE_CONFLICT`
- `REL_PREDECESSOR_MISMATCH`
- `REL_DRIFT_BLOCKED`
- `REL_PUBLISH_FAILED`
- `REL_PUBLISH_STATE_UNKNOWN`
- `REL_PARTIAL_PUBLICATION`
- `REL_VERIFICATION_FAILED`
- `REL_RESTART_FAILED`
- `REL_ROLLBACK_UNAVAILABLE`
- `REL_ROLLBACK_FAILED`
- `REL_RESUME_RECONCILIATION_REQUIRED`

## 28. Acceptance criteria

1. Approval invalidation matrix covers every immutable request field/evidence expiry.
2. Same bundle identity is proven from staging through production.
3. Failure injection before/during/after every per-place commit point yields correct state.
4. Lost provider/Studio response never causes blind duplicate publication.
5. Partial multi-place release always offers explicit reconcile/rollback/roll-forward paths.
6. Rollback target is validated and tested on disposable universes.
7. Drift unknown cannot pass strict no-drift gate.
8. Adoption never bypasses Git/art review authorities.

