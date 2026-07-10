# SPEC-22 — Traceability and Acceptance Matrix

Status: Draft, normative  
Version: 0.1.0  
Last updated: 2026-07-09  
Owners: Architecture, Quality Engineering, Product  
Depends on: SPEC-00 through SPEC-21  
Supersedes: none

## 1. Purpose

This specification is the release-facing index that proves the system described by SPEC-00 through SPEC-21 is complete, internally traceable, and testable. A feature is not complete merely because an implementation exists. It is complete only when:

1. its normative requirement has a stable requirement ID;
2. its implementation owner is known;
3. its automated or manual acceptance evidence is recorded;
4. its security and failure behavior have been exercised;
5. its documentation and migration obligations are satisfied; and
6. all applicable release gates in this specification pass.

This document does not weaken any requirement in another specification. If a row summarizes a requirement less precisely than its source specification, the source specification controls.

## 2. Traceability model

Every implemented requirement MUST be traceable through the following chain:

```text
product invariant or user story
    -> normative requirement ID
    -> architecture decision, where needed
    -> implementation component and change
    -> test or inspection evidence
    -> release artifact and version
    -> operational evidence after release
```

### 2.1 Evidence record

The release repository MUST contain a machine-readable evidence record for every candidate build:

```json
{
  "schemaVersion": 1,
  "releaseVersion": "0.8.0-beta.2",
  "logicalBuildHash": "blake3:...",
  "sourceCommit": "0123456789abcdef...",
  "generatedAt": "2026-07-09T21:00:00Z",
  "requirements": {
    "IAM-001": {
      "status": "pass",
      "evidence": ["CT-AUTH-001", "SEC-AUTH-003"],
      "artifacts": ["reports/ct-auth-001.xml"],
      "waiver": null
    }
  },
  "phaseGates": {
    "teamAlpha": "pass"
  },
  "signatures": [
    {
      "keyId": "release-signing-2026-01",
      "algorithm": "ed25519",
      "signature": "base64..."
    }
  ]
}
```

The exact signing algorithm MAY change through an approved ADR. Evidence records MUST be immutable once a release is promoted.

### 2.2 Requirement states

Each requirement has exactly one release state:

- `not_started`: no qualifying evidence exists;
- `implemented`: implementation exists but acceptance evidence is incomplete;
- `pass`: all required evidence passes on the candidate;
- `fail`: at least one required test or inspection fails;
- `not_applicable`: the release does not include the governed capability and the source specification permits omission;
- `waived`: an authorized, time-bounded waiver exists.

`not_applicable` and `waived` MUST include a reason, approver, issue link, creation time, and expiry condition. Security-critical requirements MUST NOT be waived for a stable release unless the security owner and release owner both approve and a compensating control is documented.

## 3. Test and evidence identifiers

| Prefix | Evidence type | Required contents |
|---|---|---|
| `UT-` | Unit test | component, input, expected output |
| `PT-` | Property/fuzz test | property, generator, seed or replay corpus |
| `CT-` | Contract test | producer and consumer versions, fixture |
| `IT-` | Integration test | real or qualified provider boundary |
| `E2E-` | End-to-end test | actor, environment, observable outcome |
| `SEC-` | Security test | threat, technique, expected control |
| `PERF-` | Performance test | hardware profile, data size, percentile |
| `COMPAT-` | Compatibility test | OS, Studio, toolchain, protocol versions |
| `UX-` | Usability/accessibility inspection | task, actor, assistive technology where applicable |
| `DR-` | Disaster-recovery exercise | failure, restored components, RPO/RTO result |
| `MAN-` | Controlled manual inspection | operator, procedure, screenshots or signed record |

Evidence IDs MUST be unique, stable, and linked from the source requirement in code or test metadata. A renamed test MUST retain the old evidence ID or declare an alias.

## 4. Product invariant traceability

| Invariant | Primary specifications | Minimum evidence |
|---|---|---|
| `INV-001`–`INV-004` identity, supported interfaces, and least privilege | SPEC-01, SPEC-04, SPEC-13, SPEC-18 | `CT-AUTH-*`, `SEC-AUTH-*`, `SEC-RBX-*` |
| `INV-005`–`INV-008` explicit ownership and no silent overwrite | SPEC-01, SPEC-03, SPEC-07, SPEC-09, SPEC-11 | `PT-OWN-*`, `E2E-CONFLICT-*` |
| `INV-009`–`INV-012` immutable revisions and provenance | SPEC-02, SPEC-09, SPEC-14, SPEC-17 | `PT-HASH-*`, `IT-CAS-*`, `E2E-BUILD-*` |
| `INV-013`–`INV-016` production safety and rollback | SPEC-14, SPEC-15, SPEC-18 | `E2E-REL-*`, `SEC-REL-*` |
| `INV-017`–`INV-020` Studio safety and live update | SPEC-07, SPEC-08, SPEC-09 | `E2E-STUDIO-*`, `CT-BRIDGE-*` |
| `INV-021`–`INV-024` deterministic assembly and tool pinning | SPEC-03, SPEC-11, SPEC-14 | `E2E-REPRO-*`, `COMPAT-TOOL-*` |
| `INV-025`–`INV-028` auditable control plane and locks | SPEC-16, SPEC-18, SPEC-19 | `IT-HUB-*`, `PT-LOCK-*`, `SEC-HUB-*` |
| `INV-029`–`INV-032` provider portability and offline behavior | SPEC-05, SPEC-06, SPEC-17 | `E2E-OFFLINE-*`, `IT-PROVIDER-*` |
| `INV-033`–`INV-035` compatibility and forward migration | SPEC-00, SPEC-03, SPEC-08, SPEC-20 | `COMPAT-*`, `CT-MIGRATE-*` |
| `INV-036`–`INV-038` observability, recovery, and open operation | SPEC-19, SPEC-20, SPEC-21 | `DR-*`, `MAN-GOV-*`, `E2E-EXPORT-*` |
| `INV-039`–`INV-044` no vendor runtime, public-client PKCE, OS-vault fail-closed storage, and official GitHub distribution | SPEC-01, SPEC-04, SPEC-20, SPEC-21 | `SEC-NOCENTRAL-001`, `SEC-AUTH-PUBLIC-001`, `COMPAT-SECRETSTORE-001`, `SEC-OFFICIAL-BUILD-001` |

Before team alpha, Quality Engineering MUST expand this table to one row per invariant without changing the invariant definitions in SPEC-01.

## 5. System acceptance matrix

### 5.1 Product boundary and domain model

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `DOM-001` | A project can represent one Roblox universe with one or more places, Git source, art cells, environments, and release channels. | `CT-MODEL-001`, `E2E-PROJECT-001` | team alpha |
| `INV-004` | The product never treats production as an editable source of truth. | `PT-AUTHORITY-001`, `E2E-DRIFT-001` | team alpha |
| `INV-001` | Every mutable path resolves to exactly one declared authority. | `PT-OWN-001` with generated overlapping rules | team alpha |
| `DOM-002` | A prohibited or unsupported operation fails closed with an actionable error. | `CT-ERROR-001`, `UX-ERROR-001` | public beta |
| `DOM-003` | All persisted entities round-trip without semantic loss. | `PT-MODEL-ROUNDTRIP-001` | team alpha |
| `DOM-004` | State machines reject every invalid transition. | `PT-STATE-001` | team alpha |
| `DOM-005` | Typed Roblox values preserve type identity, not merely display text. | `PT-TYPEDVALUE-001` | team alpha |
| `DOM-006` | Optimistic concurrency produces conflicts instead of last-writer-wins data loss. | `IT-CAS-001`, `E2E-CONFLICT-001` | team alpha |

### 5.2 Project manifest and configuration

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `CFG-001` | A valid `neuman.project.yaml` is parsed deterministically on every supported OS. | `COMPAT-CFG-001` | team alpha |
| `CFG-002` | Unknown required fields, duplicate ownership, invalid DataModel paths, and secrets in configuration are rejected. | `PT-CFG-VALIDATE-001`, `SEC-CFG-001` | team alpha |
| `CFG-003` | `neuman.lock.json` fully pins required tools and dependency inputs. | `CT-LOCKFILE-001`, `E2E-REPRO-001` | team alpha |
| `CFG-004` | Local-only data cannot enter a commit unless explicitly exported. | `E2E-LOCALCFG-001` | public beta |
| `CFG-005` | Schema migrations are forward-only, backed up, idempotent, and testable. | `CT-MIGRATE-001`, `DR-MIGRATE-001` | public beta |

### 5.3 Identity, authentication, and authorization

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `IAM-001` | Roblox sign-in uses the first-party OAuth authorization-code flow with PKCE and exact redirect validation. | `IT-AUTH-RBX-001`, `SEC-AUTH-001` | team alpha |
| `IAM-002` | State, nonce where applicable, PKCE verifier, and redirect correlation are unpredictable, single-use, and expire. | `PT-AUTH-CORRELATION-001`, `SEC-AUTH-002` | team alpha |
| `IAM-003` | Refresh, revocation, expiry, denied consent, and account mismatch are handled without credential disclosure. | `IT-AUTH-RBX-002`, `UX-AUTH-001` | team alpha |
| `IAM-004` | The product never requests a Roblox browser cookie or user-supplied API key. | `SEC-AUTH-003`, binary/string inspection | every release |
| `IAM-005` | GitHub user and App identities are distinct and permissions remain least-privileged. | `IT-AUTH-GH-001`, `SEC-GH-001` | public beta |
| `IAM-006` | Every Hub action is denied unless principal, project role, resource scope, and policy permit it. | `PT-AUTHZ-001`, `SEC-HUB-001` | team alpha |
| `IAM-007` | Production publish and destructive adoption require recorded step-up approval. | `E2E-APPROVAL-001`, `SEC-REL-001` | public beta |
| `IAM-008` | Stored credentials use the OS secret store; logs and support bundles redact them. | `COMPAT-SECRETSTORE-001`, `SEC-REDACT-001` | team alpha |
| `IAM-009` | Official desktop builds compile a reviewed public Roblox client ID, use S256 PKCE, contain no client secret, and do not allow runtime client-identity replacement. | `SEC-AUTH-PUBLIC-001`, binary/string inspection, release configuration record | every official release |
| `IAM-010` | A locked, unavailable, denied, or corrupt OS vault fails closed with no plaintext, SQLite, renderer, environment, or project-file token fallback. | `COMPAT-SECRETSTORE-001`, `SEC-VAULT-FAILCLOSED-001` | every official release |

### 5.4 Desktop application

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `UX-001` | Setup creates or connects a project without requiring manual file edits. | `E2E-SETUP-001`, `UX-SETUP-001` | team alpha |
| `UX-002` | The workspace clearly separates Code, Art, and Release state. | `UX-WORKSPACE-001` | team alpha |
| `UX-003` | Every destructive action presents scope, consequences, reversibility, and approval state. | `UX-DESTRUCTIVE-001`, `SEC-UI-001` | public beta |
| `UX-004` | Offline and degraded provider states are visible and do not silently discard work. | `E2E-OFFLINE-001` | public beta |
| `UX-005` | Keyboard navigation, focus order, contrast, screen-reader names, and reduced motion meet the declared accessibility baseline. | `UX-A11Y-001` | stable |
| `UX-006` | A cold start and a 10,000-change workspace remain within SPEC-05 performance budgets. | `PERF-DESK-001` | public beta |

### 5.5 Core daemon and CLI

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `CORE-001` | Desktop and CLI use the same daemon operations and domain rules. | `CT-IPC-001` | team alpha |
| `CORE-002` | An interrupted operation resumes safely or rolls back to a known state. | `PT-OPS-001`, `E2E-CRASH-001` | public beta |
| `CORE-003` | Watchers coalesce duplicate events without missing final state. | `PT-WATCH-001`, `PERF-WATCH-001` | public beta |
| `CORE-004` | The CLI supports stable machine-readable output and documented exit codes. | `CT-CLI-001` | team alpha |
| `CORE-005` | An untrusted repository cannot cause arbitrary execution through project discovery, build, or preview. | `SEC-REPO-001` | team alpha |
| `CORE-006` | Local CAS and operation records survive process and machine restart within documented guarantees. | `DR-LOCAL-001` | public beta |

### 5.6 Studio plugin and loopback bridge

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `STU-001` | The plugin proves project, universe, place, and workspace identity before mutation. | `E2E-PAIR-001`, `SEC-PAIR-001` | team alpha |
| `STU-002` | Captures contain a consistent checkpoint or fail without publishing a partial revision. | `PT-CAPTURE-001`, `E2E-CAPTURE-001` | team alpha |
| `STU-003` | Incoming changes apply through preview, validation, recorded undo, and post-apply verification. | `E2E-APPLY-001` | team alpha |
| `STU-004` | Editing state, active tools, play mode, and local dirty paths prevent unsafe automatic application. | `E2E-SAFEPOINT-001` | public beta |
| `STU-005` | A developer receives an accepted art revision in an open Studio session without reopening the place. | `E2E-LIVEART-001` | team alpha |
| `STU-006` | Failed apply restores the prior snapshot or reports a recoverable, exact repair procedure. | `E2E-APPLY-ROLLBACK-001` | public beta |
| `LBP-001` | The loopback service binds only to loopback, requires pairing, and rejects cross-origin or unauthenticated access. | `SEC-BRIDGE-001`, `COMPAT-BRIDGE-001` | team alpha |
| `LBP-002` | Protocol negotiation handles current/current-1 versions and fails cleanly otherwise. | `CT-BRIDGE-VERSION-001` | public beta |
| `LBP-003` | Ordered messages, replay protection, idempotency keys, and resume semantics prevent duplicate mutation. | `PT-BRIDGE-ORDER-001`, `E2E-BRIDGE-RESUME-001` | team alpha |
| `LBP-004` | Large transfers stream in bounded chunks, verify content hashes, and resume after disconnect. | `PERF-BRIDGE-001`, `E2E-BRIDGE-TRANSFER-001` | public beta |

### 5.7 Art cells, terrain, service state, assets, and packages

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `ART-001` | A cell capture has a stable identity, complete native payload, semantic index, dependency set, and deterministic root. | `PT-ART-HASH-001`, `E2E-ART-CAPTURE-001` | team alpha |
| `ART-002` | Repeated capture of unchanged Studio state produces the same ArtState root. | `E2E-ART-DETERMINISM-001` | team alpha |
| `ART-003` | A revision cannot advance a channel head unless its expected parent matches. | `IT-ART-CAS-001` | team alpha |
| `ART-004` | Three-way merge automatically combines only disjoint safe changes; ambiguous changes become explicit conflicts. | `PT-ART-MERGE-001`, `E2E-ART-CONFLICT-001` | team alpha |
| `ART-005` | Cross-cell references preserve resolvable identity or are rejected with the referencing path. | `PT-ART-REF-001` | public beta |
| `ART-006` | A checkout never overwrites local dirty work without an explicit resolution. | `E2E-ART-CHECKOUT-001` | team alpha |
| `ART-007` | Native payload loss, unknown classes/properties, and semantic uncertainty are surfaced, not normalized away. | `CT-NATIVE-CORPUS-001` | public beta |
| `TAS-001` | Terrain tile ownership is non-overlapping and every changed tile has an exclusive lease when policy requires it. | `PT-TERR-OWN-001`, `IT-TERR-LOCK-001` | public beta |
| `TAS-002` | Terrain round-trips through the selected encoding within its declared exactness contract. | `CT-TERR-ROUNDTRIP-001` | public beta |
| `TAS-003` | Lighting, Atmosphere, Sound, and other registered service state round-trip through an explicit property registry. | `CT-SVC-001` | public beta |
| `TAS-004` | External asset dependencies retain asset ID, type, permission status, and revision context. | `IT-ASSET-001` | public beta |
| `TAS-005` | Package links are not silently flattened or upgraded during capture or assembly. | `CT-PACKAGE-001` | public beta |

### 5.8 Git, Rojo, and GitHub

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `GIT-001` | Dirty, detached, conflicted, missing-upstream, and divergent states are detected before mutation. | `CT-GIT-STATE-001` | team alpha |
| `GIT-002` | User work is never reset, cleaned, rebased, force-pushed, or committed without explicit action. | `SEC-GIT-001`, `E2E-GIT-SAFETY-001` | every release |
| `GIT-003` | Builds run from an isolated, immutable commit/worktree. | `E2E-BUILD-WORKTREE-001` | team alpha |
| `GIT-004` | The pinned Rojo binary and project file are validated before serve/build. | `COMPAT-ROJO-001`, `CT-TOOLCHAIN-001` | team alpha |
| `GIT-005` | Rojo-owned and art-owned paths cannot overlap. | `PT-OWN-ROJO-001` | team alpha |
| `GIT-006` | Source maps preserve diagnostics and ownership identity through assembly. | `CT-SOURCEMAP-001` | public beta |
| `GHA-001` | GitHub App permissions are no broader than SPEC-12 and installation ownership is verified. | `SEC-GH-PERMS-001` | public beta |
| `GHA-002` | Webhook signatures, delivery replay, and reconciliation are correct under duplication and reordering. | `PT-GH-WEBHOOK-001`, `IT-GH-RECONCILE-001` | public beta |
| `GHA-003` | Required status checks publish immutable build/release evidence and respect branch protection. | `E2E-GH-CHECK-001` | public beta |

### 5.9 Roblox provider integration

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `RBX-001` | Only documented first-party OAuth, Open Cloud, Studio, plugin, and supported CLI/tool interfaces are used. | `SEC-RBX-SURFACE-001`, dependency and network inspection | every release |
| `RBX-002` | Universe and place discovery returns only resources the authenticated principal can access. | `IT-RBX-DISCOVERY-001` | team alpha |
| `RBX-003` | The tool never claims a supported OAuth full-place download when Roblox does not expose one. | `MAN-RBX-CAPABILITY-001` | every release |
| `RBX-004` | Studio-assisted assembly uses a fixed signed runner and declarative signed manifest. | `SEC-RUNNER-001`, `E2E-ASSEMBLY-001` | team alpha |
| `RBX-005` | Publish mode is explicit, credentials match the target universe, and the result has a verifiable receipt. | `E2E-PUBLISH-001`, `SEC-RBX-PUBLISH-001` | team alpha |
| `RBX-006` | Provider throttling, authorization errors, moderation failures, and transient outages are classified and retried only when safe. | `IT-RBX-FAILURE-001` | public beta |
| `RBX-007` | A selected server restart policy is displayed, approved, executed, and audited. | `E2E-RBX-RESTART-001` | stable |

### 5.10 Build, release, rollback, and drift

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `BLD-001` | LogicalBuildHash is a canonical function of all declared inputs and policies. | `PT-BUILD-HASH-001` | team alpha |
| `BLD-002` | Identical declared inputs produce identical logical build identity, and semantic equivalence is reported separately from byte equivalence. | `E2E-REPRO-001` | team alpha |
| `BLD-003` | The build DAG records every input, tool version, transformation, validation, output, and log. | `CT-PROVENANCE-001` | team alpha |
| `BLD-004` | A failed validation cannot yield a publishable bundle. | `PT-GATE-001` | team alpha |
| `BLD-005` | Studio-native assembly runs in a clean controlled session with bounded time and artifact verification. | `E2E-ASSEMBLY-001`, `PERF-ASSEMBLY-001` | public beta |
| `REL-001` | Staging and production consume the exact same immutable release-bundle identity. | `E2E-PROMOTE-001` | team alpha |
| `REL-002` | Every place publish has preflight, approval, commit point, receipt, verification, and audit records. | `E2E-PUBLISH-001` | team alpha |
| `REL-003` | A multi-place release is a visible saga; partial completion is never reported as atomic success. | `E2E-MULTIPLACE-001` | public beta |
| `REL-004` | Rollback republishes a known prior bundle and records a new deployment; it does not rewrite history. | `E2E-ROLLBACK-001` | public beta |
| `REL-005` | Publish concurrency and stale-plan detection prevent overwriting an unexpected remote version. | `E2E-PUBLISH-RACE-001` | public beta |
| `REL-006` | Production drift reports expected and observed release evidence with confidence and source. | `E2E-DRIFT-001` | public beta |
| `REL-007` | Adopting drift creates a new explicit revision or commit proposal; it never rewrites the prior release. | `E2E-DRIFT-ADOPT-001` | public beta |

### 5.11 Hub and storage

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `HUB-001` | Mutating APIs are authenticated, authorized, idempotent, and audited. | `CT-HUB-API-001`, `SEC-HUB-API-001` | team alpha |
| `HUB-002` | Revision-head compare-and-swap and lease acquisition are serializable under contention. | `PT-HUB-CAS-001`, `IT-HUB-CONTENTION-001` | team alpha |
| `HUB-003` | Lock leases expire, renew, release, and batch-acquire atomically according to SPEC-16. | `PT-LOCK-001`, `IT-LOCK-001` | team alpha |
| `HUB-004` | Event replay, reconnect, and gap recovery converge clients on authoritative state. | `PT-EVENT-001`, `E2E-RECONNECT-001` | public beta |
| `HUB-005` | Database changes and external events use an outbox or equivalent no-loss transaction pattern. | `IT-OUTBOX-001`, `DR-OUTBOX-001` | public beta |
| `STO-001` | CAS put is atomic, immutable, deduplicated, and verified on read. | `PT-CAS-001`, `IT-CAS-CORRUPTION-001` | team alpha |
| `STO-002` | Uploads and downloads resume safely and never expose a partial object as complete. | `E2E-CAS-RESUME-001` | public beta |
| `STO-003` | Garbage collection preserves every reachable release, revision, retention pin, and legal hold. | `PT-GC-001` | stable |
| `STO-004` | Provider export contains all metadata, objects, hashes, and verification instructions needed to restore elsewhere. | `E2E-EXPORT-001` | public beta |
| `STO-005` | Experimental Lore support remains disabled unless SPEC-17 promotion gates pass. | `CT-LORE-FLAG-001`, `PERF-LORE-001` | every release containing Lore adapter |

### 5.12 Security, operations, testing, and governance

| Requirement | Acceptance condition | Required evidence | Release gate |
|---|---|---|---|
| `SEC-001` | Threat model is reviewed against every changed trust boundary. | `MAN-THREAT-REVIEW-001` | every release |
| `SEC-002` | Windows/macOS artifacts, update metadata, and fixed runners are signed and verified before execution. | `SEC-SIGNING-001` | public beta |
| `SEC-003` | Native model inputs, archives, manifests, and network payloads enforce size, path, type, and resource limits. | `PT-UNTRUSTED-INPUT-001`, `SEC-PARSER-001` | team alpha |
| `SEC-004` | Dependency and build provenance are available as an SBOM and attestation. | `MAN-SBOM-001`, `CT-ATTEST-001` | public beta |
| `SEC-005` | Every official binary originates from a protected GitHub workflow, signed annotated tag, exact commit, native platform signature, updater signature, SHA-256 manifest, and repository-bound artifact attestation. | `SEC-OFFICIAL-BUILD-001`, `CT-ATTEST-001`, clean-VM signature verification | every official release |
| `OPS-001` | Critical user operations have correlated logs, metrics, traces where applicable, and an audit event. | `E2E-OBS-001` | public beta |
| `OPS-002` | Provider outages enter documented degraded modes and preserve queued work. | `E2E-DEGRADED-001` | public beta |
| `OPS-003` | Hub metadata and objects restore within the declared RPO and RTO. | `DR-HUB-001` | stable |
| `OPS-004` | A user can export and restore a project between local/self-hosted deployments without a vendor service. | `E2E-EXPORT-001`, `DR-SELFHOST-001` | public beta |
| `TST-001` | The native-format corpus, compatibility matrix, fuzz tests, and provider contract tests pass for the candidate. | test suite aggregate | every release |
| `TST-002` | Flaky tests cannot be silently retried into green; quarantine is visible and expiring. | CI policy inspection | every release |
| `OSS-001` | Source, specifications, protocol schemas, build instructions, and contribution policy are public and versioned. | `MAN-GOV-001` | public beta |
| `OSS-002` | Local and self-hosted modes use documented protocols and portable data formats; no NeuMan-operated runtime or central database is required. | `CT-PORTABILITY-001`, `SEC-NOCENTRAL-001` | every official release |

## 6. Required end-to-end acceptance stories

These stories are mandatory system tests. Mocks MAY be used for fault injection, but the primary happy-path run MUST use qualified real integrations in a disposable test universe and test repository.

### E2E-SETUP-001 — New project and Roblox OAuth

**Preconditions:** Clean supported workstation; test GitHub repository; disposable Roblox universe with at least two places; no existing local credentials.

**Procedure:**

1. Install the signed desktop application and Studio plugin.
2. Create a project from the repository.
3. Sign in with Roblox through first-party OAuth with PKCE.
4. Select the authorized universe and bind both places.
5. Configure code ownership, one art cell, staging, and production.
6. Reopen the application after restart.

**Pass conditions:**

- credentials are stored only in the OS secret store;
- neither cookies nor user API keys are requested;
- the correct Roblox account and scopes are visible;
- manifests validate and contain no secret;
- the project, bindings, and identity survive restart;
- an authorization denial or account mismatch produces no partial binding.

### E2E-CODE-LIVE-001 — Git code to an open Studio session

1. Start from clean `main` and an open bound Studio place.
2. Change a Luau source file on a feature branch.
3. Run validation and start the pinned Rojo session.
4. Observe the code update in Studio.
5. introduce a syntax error, then repair it.

**Pass conditions:** The valid update appears without reopening Studio; the error is attributed to the source file and blocks a releasable build; repairing it restores green state; no art-owned instance is modified.

### E2E-LIVEART-001 — Artist capture to developer Studio without reload

1. Artist A and Developer B open separately paired Studio sessions for the same place and project.
2. A acquires the required cell lease, modifies a cell, and captures a checkpoint.
3. A reviews and accepts the proposed revision.
4. B receives the accepted-head event while Studio remains open.
5. B previews and applies the revision.

**Pass conditions:** The revision includes native snapshot and semantic index; B sees author, cell, paths, change summary, and warnings; apply occurs inside one undo recording; post-apply verification succeeds; the place is never reopened; B's code-owned and unrelated art paths are untouched.

### E2E-ART-CONFLICT-001 — Concurrent art edits

1. A and B begin from revision `R0`.
2. Both edit the same property or terrain tile incompatibly.
3. A accepts `R1`.
4. B attempts to accept a revision based on `R0`.

**Pass conditions:** The channel CAS rejects B's stale update; the UI shows base, ours, and theirs; no silent last-writer-wins overwrite occurs; B may discard, rebase, or resolve into a new revision with both parents recorded as applicable.

### E2E-BUILD-001 — Immutable release build

1. Select a Git commit, accepted art heads, lockfile, manifest, and target configuration.
2. Build twice on clean qualified workers.
3. Inspect provenance and output.

**Pass conditions:** Both runs have the same LogicalBuildHash; all declared inputs and tool hashes are present; semantic roots match; byte equivalence is reported truthfully; no dirty workspace input is consumed; an invalid ownership overlap makes both builds non-publishable.

### E2E-PROMOTE-001 — Staging proof and production promotion

1. Publish a release bundle to every staging place.
2. Complete configured staging checks and approvals.
3. Promote the same release to production.

**Pass conditions:** Staging and production receipts reference the exact same bundle and place artifacts; production performs fresh preflight checks; no rebuild occurs between environments; deployment markers and audit records identify actor, approval, target, provider result, and version.

### E2E-MULTIPLACE-001 — Partial multi-place publication

1. Configure a three-place release plan with place B dependent on place A.
2. Make publication to place B fail after A commits.

**Pass conditions:** A remains recorded as committed; B is failed; C is not started if dependency policy requires; the release is `partially-published`, never `published`; the operator receives valid roll-forward and rollback plans; retry cannot duplicate A's commit.

### E2E-ROLLBACK-001 — Roll back a bad deployment

1. Deploy bundle `N` after bundle `N-1`.
2. Trigger rollback after a failing production check.

**Pass conditions:** The system publishes the retained `N-1` artifact as a new deployment operation; it verifies the resulting version; history retains both deployments and the rollback reason; rollback never changes Git or art history.

### E2E-DRIFT-ADOPT-001 — Detect and adopt an emergency Studio edit

1. Publish a known release.
2. Make an authorized emergency production edit outside the normal flow.
3. Detect drift through the strongest available evidence.
4. Capture the production state from Studio and choose adoption.

**Pass conditions:** Confidence and observation method are shown; adoption creates a proposal based on the last known release; conflicts are explicit; the prior release stays immutable; nothing automatically pushes to `main` or marks the drift approved.

### E2E-OFFLINE-001 — Offline editing and reconnection

1. Start from a synchronized project, then remove network access.
2. Continue local code edits and an allowed local art capture.
3. Restart the desktop application.
4. Restore network access after the remote art head has independently advanced.

**Pass conditions:** Local work persists; remote-dependent actions are marked queued or unavailable; reconnection fetches authoritative state; the stale local art proposal becomes a conflict or rebase flow; no queued production publish executes without renewed preflight and approval.

### DR-HUB-001 — Control-plane disaster recovery

1. Restore the most recent approved PostgreSQL and object-store backups into an isolated environment.
2. Reconcile outbox, references, and object hashes.
3. Exercise project read, revision checkout, lock, build evidence, and release history flows.

**Pass conditions:** Measured RPO and RTO meet SPEC-19; every referenced object verifies; orphan handling is reported; audit records remain ordered and attributable; the exercise produces a signed recovery report.

### E2E-EXPORT-001 — Local/self-hosted portability

1. Export a project from a local or self-hosted deployment including metadata, revisions, release bundles, audit records permitted by policy, and CAS objects.
2. Verify the export offline.
3. Import it into a clean local or self-hosted deployment with no NeuMan-operated service available.

**Pass conditions:** Hashes and signatures verify; accepted heads and release history match; the project can build from retained inputs; provider-specific secrets are omitted; missing externally controlled assets are clearly listed.

## 7. Phase 0 architecture decision gates

No production implementation may proceed past a gate whose required Phase 0 decision is unresolved.

| ID | Decision or experiment | Required output | Blocks |
|---|---|---|---|
| `P0-01` | Roblox PKCE | Windows/macOS public-client sign-in, OIDC validation, refresh rotation/revocation, no client secret, user/group resource proof | OAuth UX and team alpha |
| `P0-02` | Resource discovery | Exact universe/place/creator/permission report for user/group ownership; wrong and unauthorized IDs fail | automatic binding and publish preflight |
| `P0-03` | Native cell round-trip | Cross-session native/semantic/reference/dependency equivalence report and unsupported-class catalog | stable art fidelity claim |
| `P0-04` | Terrain | Selected encoding ADR plus solid/liquid/material/bounds, rollback, size, memory, and compatibility evidence | terrain beta |
| `P0-05` | Loopback transfer | Windows/macOS header/fallback qualification, 96 MiB transfer, reconnect/loss/corruption/replay tests, bounded memory | team alpha Studio bridge |
| `P0-06` | Studio CLI runner | Documented `RunScript` loads local/published targets, authenticates a fixed manifest, performs only command-bar-context validation, reports unavailable capabilities including `PluginOrOpenCloud` APIs, returns a receipt, and exits deterministically | CLI-assisted validation and launch supervision |
| `P0-07` | `SavePlaceAsync` | Disposable-target publish, API setting/permission proof, Team Create block, prompt matrix, lost-response reconciliation, version evidence | Studio-assisted publish |
| `P0-08` | Rojo 7.7 | Live code, build, opt-in two-way/syncback, source maps, ownership exclusions, Mesh/CSG replacement, and references report | code sync/build alpha |
| `P0-09` | Storage benchmark | Reproducible Git/Git LFS/S3/Lore corpus benchmark, locking/failure analysis, recovery, and export test | v1 provider decision and Lore promotion |
| `P0-10` | Determinism | Raw `.rbxm/.rbxl` byte-stability measurements, semantic-fingerprint comparison, and finalized verification-level ADR | reproducibility claims |
| `P0-11` | Open Cloud Luau Execution | Exact place/version task, bounded binary inputs, fixed runner, task/log polling, signed receipt, five-minute lifetime handling, timeout/lost-response reconciliation, `SavePlaceAsync` version evidence, and proof the operator key never enters desktop/plugin/Hub | high-fidelity operator-owned CI assembly and publish |

Each row MUST result in an ADR with one of `accepted`, `rejected`, or `deferred`. `Deferred` MUST name the user-visible feature that remains disabled or experimental.

## 8. Release readiness gates

### 8.1 Prototype gate

- `P0-01` through `P0-07` have evidence sufficient to validate or narrow the public desktop architecture; `P0-11` is additionally required before enabling the operator-owned high-fidelity CI profile.
- No claim is made that unsupported place download, semantic merge, or unattended publish works.
- Native model and terrain experiments retain their original source corpus and tool versions.

### 8.2 Team alpha gate

- Setup, OAuth, Git/Rojo live code, Studio pairing, cell capture, accepted-head notification, preview/apply, immutable build, and a controlled publish pass end to end.
- Every team-alpha row in Section 5 is `pass`.
- P0 gates required by included features are accepted.
- Security review covers OAuth, loopback, repository trust, native input parsing, CAS, and publishing.
- A tested backup exists for every non-reconstructible team project artifact.
- Data loss, wrong-place publish, silent overwrite, or credential disclosure are release blockers.

### 8.3 Public beta gate

- Every public-beta row is `pass` or has a formally approved, user-visible limitation.
- Windows and macOS installers are signed; updater verification and rollback are tested.
- The official GitHub workflow proves signed tag/commit identity, protected signing and publish environments, native signatures/notarization, updater signatures, checksums, SBOM, and repository-bound attestations from a clean build.
- GitHub App, Hub concurrency, large-object resume, offline recovery, multi-place saga, rollback, drift adoption, export, and degraded modes pass.
- Threat model, privacy notice, security reporting, support bundle, migration, and operator documentation are public.
- A third party can self-host the Hub and import an export using only public documentation.

### 8.4 Stable gate

- Every stable row is `pass`; no unresolved P0 decision affects a stable claim.
- The complete Studio/OS/tool compatibility matrix passes.
- Native and terrain fidelity targets are met for the declared support set.
- Accessibility, SLO alerting, disaster recovery, garbage collection, signing, SBOM, provenance, and upgrade/rollback drills pass.
- At least two real projects have completed the documented production-release and rollback procedures.
- There are no open critical vulnerabilities or known silent-corruption paths.

## 9. Definition of done for a system

A system specification is implemented only when all of the following are true:

- every normative requirement has an owner and evidence ID;
- API schemas, error codes, state machines, quotas, and timeouts match the specification;
- happy path, denial, timeout, disconnect, replay, concurrency, corruption, and recovery are tested where applicable;
- logs, metrics, traces, and audit events are defined and redacted;
- secrets and permission scopes are reviewed;
- migrations are forward/backward compatibility-tested according to SPEC-00;
- accessibility and operator UX are inspected;
- documentation includes setup, normal operation, recovery, and uninstall/export;
- performance is measured on the reference hardware and corpus;
- any deviation has an accepted ADR and spec amendment;
- the evidence record for the candidate reports `pass`.

## 10. Requirement coverage audit

The repository MUST provide a CI command equivalent to:

```text
neuman-spec lint --spec-root . --evidence build/evidence.json
```

The linter MUST fail when:

- a requirement ID is duplicated;
- a normative requirement lacks an ID after the specification reaches `Accepted` status;
- an evidence ID is referenced but absent;
- a test references a nonexistent or superseded requirement without an alias;
- a release-required requirement is `fail`, `implemented`, or `not_started`;
- a waiver is expired, unsigned, or lacks a compensating control where required;
- incompatible specification versions are selected;
- a generated schema differs from its normative checked-in source;
- a stable release omits an applicable compatibility or disaster-recovery result.

## 11. Cross-system consistency rules

1. The authority resolver in the manifest validator, daemon, Studio plugin, build engine, and Hub MUST produce the same result for the same normalized path and project version.
2. ArtState root calculation in Studio, desktop, worker, and Hub MUST pass the same golden vectors.
3. LogicalBuildHash calculation in desktop, CLI, worker, and release service MUST pass the same golden vectors.
4. The loopback protocol and Hub event stream MUST use distinct credentials and trust domains even if they share envelope libraries.
5. A local plugin capture is not an accepted art revision until the configured acceptance policy and head CAS succeed.
6. A successful build is not a release; a release is not a deployment; a deployment is not verified until provider evidence and configured checks pass.
7. Staging proof MUST reference the production candidate bundle exactly; changing an input creates a new candidate and invalidates prior proof as configured.
8. A production observation is evidence, not authority. Adoption MUST create new versioned source state.
9. Provider retry policies MUST share the idempotency and commit-point semantics defined by the owning provider specification.
10. Optional team features MUST preserve the same exportable domain objects and public protocols across local, self-hosted, and explicitly selected third-party compatible operation; no official NeuMan runtime service exists.

## 12. Non-negotiable product decisions

The following decisions are part of the baseline architecture and require a superseding accepted ADR plus updates to every affected specification:

- Desktop shell: Tauri 2 with React/TypeScript UI and a Rust privileged core.
- Core daemon, Hub, workers, CLI, and protocol-critical libraries: Rust by default.
- Studio integration: Luau plugin connected through an authenticated loopback bridge.
- Code authority: Git commit plus pinned Rojo mapping and toolchain.
- Art authority: immutable, cell-scoped native Studio snapshots plus semantic indexes and explicit accepted heads.
- Production: deployment target and observable state, never the primary editable source of truth.
- Authentication: Roblox first-party OAuth for user delegation; no `.ROBLOSECURITY` cookie collection.
- Publishing: explicit supported mode with immutable bundle, approvals, commit-point-aware orchestration, receipts, verification, and rollback.
- Merge unit: cell/revision, not raw binary-file line merge; semantic merge is conservative and conflicts are first-class.
- Storage: open content-addressed provider interface, with Git LFS/S3-compatible storage as the baseline; Lore remains experimental until promoted by evidence.
- Promotion: the exact same release bundle moves from staging to production.
- Open-source posture: public schemas/protocols, portable exports, optional self-hostable Hub, protected GitHub signed/provenance release process, no vendor central database, and no hosted-only proprietary project format.

## 13. Final acceptance authority

The release owner decides whether a candidate advances only after Quality Engineering signs the evidence record and the security owner signs all applicable security gates. For production-affecting stable releases, a failed non-waivable row, missing provider qualification, or unknown artifact provenance is an automatic rejection.

Human approval is not a substitute for missing technical evidence. Automated evidence is not a substitute for explicit approval where policy requires a human decision. Both are required when specified.
