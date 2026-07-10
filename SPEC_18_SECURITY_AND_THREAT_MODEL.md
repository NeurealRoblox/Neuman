# SPEC-18 — Security and Threat Model

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: all component specifications

## 1. Security objectives

NeuMan MUST protect:

1. Roblox, GitHub, Hub, and signing credentials.
2. Private source code, native art, DCC sources, project metadata, and audit data.
3. Integrity of art revisions, builds, release bundles, approvals, targets, and deployments.
4. Availability of local work and recoverability of accepted/released content.
5. User control over production-impact actions.
6. Tenant/project isolation in Hub.
7. Software supply chain and update integrity.

NeuMan cannot protect a fully compromised OS user account or malicious infrastructure administrator from every action, but it MUST minimize credential exposure, make high-impact actions explicit, and preserve tamper-evident audit/provenance.

## 2. Security principles

- Least privilege and minimum scopes.
- Deny by default.
- Validate at every trust boundary.
- Exact target and immutable-input binding.
- No secret in plugin, repository, logs, URLs, or process args.
- Native content is untrusted until validated.
- No arbitrary code execution from project/Hub.
- Defense in depth; UI checks never substitute core/server authorization.
- Safe failure and explicit unknown state.
- Reproducible evidence and signed distribution.
- Data minimization and user consent.

## 3. Protected assets

### Secrets

- OAuth access/refresh/ID token material;
- GitHub App private key/webhook secret/installation/user tokens;
- Hub session/service-account tokens;
- DB/object-store/KMS credentials;
- code-signing/update keys;
- pairing/IPC secrets.

### High-integrity data

- project manifest/ownership/policy;
- art state/revisions/head;
- build inputs/results/provenance;
- release bundle/approvals/waivers;
- target IDs/predecessor/drift evidence;
- deployment/rollback receipts;
- audit stream.

### Confidential data

- private repos/files;
- native models/terrain/DCC;
- previews/logs/support bundles;
- memberships/account linkage.

## 4. Threat actors

- malicious internet attacker;
- malicious/compromised local process under same or another OS user;
- malicious repository contributor;
- compromised GitHub/Roblox/Hub account;
- malicious project member with limited role;
- malicious Hub tenant attempting cross-tenant access;
- compromised dependency/update server;
- compromised object-store URL/object;
- negligent user selecting wrong target;
- infrastructure administrator (privileged threat);
- accidental corruption/race/provider outage.

## 5. Trust boundaries

1. Desktop renderer → Rust command boundary.
2. CLI/UI → local daemon IPC.
3. Studio plugin → loopback bridge.
4. Repository/filesystem → build engine.
5. Daemon → Git/Rojo/Studio child processes.
6. Daemon → Roblox/GitHub/Hub/provider internet APIs.
7. Hub API → tenant DB/object store/workers.
8. Builder/publisher → release control plane.
9. Update artifacts → installed executable/plugin.

Every boundary has authentication, authorization where applicable, schema/size validation, logging/redaction, timeout, and failure policy.

## 6. Threats and controls: OAuth/accounts

| Threat | Controls |
|---|---|
| Authorization CSRF | Random state, one transaction, exact callback |
| Code interception | PKCE S256, system browser, loopback/custom URI validation |
| ID token substitution | Signature/issuer/audience/nonce/expiry validation |
| Refresh race/reuse | Serialized atomic rotation, recovery record, reuse detection |
| Token theft at rest | OS keychain/secret manager, no config/logs |
| Scope escalation | Minimum scope UI, reconsent, endpoint authorization |
| Username impersonation | Immutable provider subject keys |
| Account-link takeover | Authenticate both accounts, collision review, audit |
| Session fixation | New session/token IDs after auth/link/step-up |
| Revoked token loop | One refresh/retry, reauth-required terminal state |

## 7. Threats and controls: local IPC/loopback

| Threat | Controls |
|---|---|
| Another OS user commands daemon | Named-pipe ACL/Unix socket `0600`, peer identity |
| Browser DNS rebinding reaches bridge | Numeric loopback bind/URL, Host allowlist, no CORS |
| Malicious local webpage sends commands | Pair credential, session/context binding, no unauthenticated mutation |
| Replay/duplicate apply | Sequence IDs, idempotency receipts, context version |
| Pair code brute force | Random challenge, approval, attempt limit, expiry |
| Credential in URL/log | Header/first-frame only, redaction |
| Oversized/base64 bomb | offer validation, chunk/hash/size/decompression bounds |
| Wrong Studio place | exact universe/place/project/channel context on command |
| Malicious local same-user process | Pairing approval and scoped credential reduce risk; same-user compromise remains documented residual risk |

## 8. Threats and controls: repository/build

| Threat | Controls |
|---|---|
| Git hook execution | off by default; project trust/capability if future |
| Shell injection | no shell; argument arrays; sanitized env |
| Credential theft via remote/filter | trust review, ephemeral helper, no secret args |
| Symlink/junction/path traversal | canonical root, no-escape checks, safe extraction |
| Malicious build script | fixed build DAG/runner; no arbitrary project execution |
| Malicious Luau in art | scripts/classes rejected, native scan before/inside Studio |
| Dependency substitution | exact pins/checksums/provenance/allowlisted source |
| Cache poisoning | key includes implementation/input/tool; output hash/provenance verify |
| LFS pointer confusion | pointer detection/materialization/hash check |
| Source secret inclusion | secret scanner, output/bundle content policy |

## 9. Threats and controls: native Roblox content

Threats:

- hidden scripts/backdoors;
- unexpected capabilities/sandbox changes;
- malformed/corrupt `.rbxm` triggering parser/engine failure;
- excessive instance/buffer/resource exhaustion;
- external asset/package content change;
- dangling/retargeted references;
- unsupported opaque property loss;
- native serialization incompatibility.

Controls:

- size/hash/media/schema checks;
- offline structural parser in memory/resource limits when used;
- engine deserialize only in controlled Studio operation;
- class/script/asset/reference/capability policy before and after deserialize;
- detached staging before parenting;
- compatibility corpus/migration review;
- no user-provided native payload in logs;
- recovery/rollback transaction;
- immutable accepted original bytes.

## 10. Threats and controls: Hub/API

| Threat | Controls |
|---|---|
| IDOR/cross-tenant | project-scoped auth before existence disclosure, row constraints/tests |
| SQL injection | typed queries/parameters, no string SQL from input |
| SSRF | provider host allowlists, no arbitrary webhook URLs |
| Webhook forgery | raw-body HMAC, delivery replay record |
| Object hash enumeration | authorization reference, signed short URL |
| Presigned URL replay | short expiry, method/object/size scope, TLS |
| Lock race/split brain | DB transaction/unique constraints/server time/base validation |
| Approval replay | exact request hash/policy/actor/time, consume/invalidate rules |
| Idempotency collision | principal/project/route/request-hash binding |
| Event injection | outbox after transaction, event schema/hash, auth subscriptions |
| Resource exhaustion | rate/quotas/body/time/concurrency limits/fair queues |
| Admin abuse | separation, audit, secret manager, least DB/object roles |

## 11. Threats and controls: release/publish

| Threat | Controls |
|---|---|
| Wrong universe/place | exact IDs, creator/name display, preflight, typed confirmation |
| Stale predecessor overwrites newer publish | drift/predecessor check and release lease |
| Approval used for changed bundle | approval binds immutable request hash |
| Public app steals API key | no key input/transport/storage |
| Blind retry double-publishes | commit-point model, external reconciliation |
| Partial release hidden | per-place saga states/receipts/rollback-required |
| Malicious runner code | fixed signed runner/declarative signed manifest |
| Rollback to unsafe old code/data | rollback compatibility warnings/gates/approval |
| Unauthorized server restart | OAuth scope + NeuMan role/approval + impact display |

## 12. Cryptography

- TLS 1.2 minimum, TLS 1.3 preferred for Hub/providers.
- BLAKE3-256 for internal content identity, not passwords/MAC by itself.
- SHA-256 for external checksums/LFS/public release artifacts.
- Ed25519 recommended for NeuMan manifest/provenance/update signatures; platform code signing also required.
- HMAC-SHA-256 for webhook verification where provider defines it.
- Password hashing not present in v1 Hub; if introduced, Argon2id through reviewed library.
- Randomness from OS CSPRNG; Roblox plugin uses documented cryptographic/guid facilities within capability limits for non-secret IDs, daemon provides secrets.
- JSON canonicalization RFC 8785 before domain signatures.
- Standard maintained libraries only; no custom cipher/protocol.

Key separation by purpose/environment. Signatures include context/domain/version to prevent cross-protocol use.

## 13. Signing keys

Classes:

- application code-signing key;
- update metadata key (offline/root plus online targets recommended);
- Hub audit/provenance key;
- local builder key optional;
- release receipt key;
- Git commit signing user/bot key.

Keys have owner, storage, algorithm, ID, creation, rotation, expiry, revocation, backup/recovery, and incident procedure. Private signing keys never live in source repo or general CI logs/artifacts.

## 14. Secrets management

Desktop: OS keychain.  
Hub: cloud/on-prem secret manager with workload identity preferred.  
CI: repository/environment secret only for operator-owned key; protected environments and no fork exposure.  
Plugin: only scoped local pairing credential.

Rotation:

- support current/previous webhook/OIDC/audit keys during bounded overlap;
- revoke compromised session families;
- test rotation regularly;
- secret metadata in diagnostics, never value.

## 15. Authorization and least privilege

- Separate project admin, production approver, release executor, Hub operator.
- Human approvals cannot be issued by service accounts.
- Provider permission and NeuMan permission both required.
- Production step-up/recent auth.
- Hub object authorization independent of hash.
- Background daemon cannot release to production without approved release command.

## 16. Secure coding requirements

- Memory-safe Rust for privileged local/server components.
- TypeScript strict mode; no renderer Node/shell access.
- Luau strict/typechecked where tooling allows.
- Input parsing with size/depth/count bounds.
- No panic/crash on untrusted input; typed errors.
- Unsafe Rust requires documented justification, review, tests.
- Dependency review, lockfiles, vulnerability scanning, license policy.
- Static analysis, lint, fuzzing, property tests.
- Security-sensitive code ownership/review.

## 17. Software supply chain

- Pinned dependencies/toolchains with checksums.
- Reproducible/controlled CI builds where possible.
- Signed Windows/macOS installers and update manifests.
- SBOM per release.
- Provenance for release artifacts.
- Protected release workflow requiring review.
- Artifact verification published separately.
- Plugin package checksum/version tied to app compatibility.
- No auto-update from GitHub latest asset without signed metadata.

## 18. Privacy/data protection

- Minimum external subject/profile data.
- No Roblox user profiling, data sale, or AI training use.
- Project content not telemetry.
- Opt-in telemetry with documented fields/retention.
- Support bundle preview/redaction.
- Data export/deletion and audit pseudonymization.
- Regional/retention deployment configuration.

## 19. Logging/redaction

Never log:

- Authorization/Cookie/X-API-Key headers;
- tokens/codes/verifiers/nonces/secrets;
- signed URLs query strings;
- native model/source/DCC bytes;
- environment variables wholesale;
- full private paths in telemetry;
- user content unless explicit local debug with consent.

Central redaction library with key-name and value-pattern filters; tests use canary secrets to ensure no sink leakage.

## 20. Denial of service controls

- HTTP/IPC/message/chunk/object limits;
- decompression preflight;
- CPU/memory/concurrency quotas;
- bounded queues/backpressure;
- timeouts/cancellation;
- fair per-project/provider scheduling;
- parser recursion/count limits;
- expensive diff/preview async jobs;
- circuit breakers for providers.

## 21. Security modes

- Development mode visibly non-production, loopback/insecure allowances, cannot target production-impact environment.
- Standard mode safe defaults.
- High-security mode requires Hub, two-person production, signed commits/bundles, strict drift, two durable object copies, telemetry off, short sessions.

Unsafe overrides are scoped, time-limited, audited, and cannot disable core integrity/target/auth checks.

## 22. Vulnerability management

- `SECURITY.md` with private reporting channel.
- Acknowledge target 2 business days.
- Triage severity/CVSS plus production impact.
- Embargoed fix/release/advisory process.
- Supported version list.
- Credential/key rotation when exposure possible.
- Notify Roblox/GitHub/provider under their programs if platform issue.
- Post-incident review and spec/test update.

## 23. Incident response

Playbooks:

- OAuth token leak;
- signing/update key compromise;
- malicious plugin/update;
- cross-tenant object/auth breach;
- unauthorized production publish/restart;
- corrupt/missing accepted art objects;
- GitHub App/private key/webhook compromise;
- ransomware/local cache/repo loss.

Common steps: contain, revoke/fence, preserve evidence, assess scope, restore/rotate, communicate, verify, retrospective.

## 24. Residual risks

- Same-user local malware can inspect process/plugin settings and act as user.
- Roblox Studio/plugin/platform changes may introduce new behavior before compatibility test.
- Hub infrastructure admin can access data absent customer-side encryption.
- External assets can change/moderate outside NeuMan.
- Team Create cannot be universally write-locked by plugin.
- Manual publication can produce incomplete evidence.

UI/docs disclose these without overstating guarantees.

## 25. Security gates

Before public beta:

- formal threat-model review;
- OAuth/app policy review;
- independent penetration test for Hub/desktop bridge/update;
- fuzzing coverage for schemas/protocol/native parsers;
- secrets/redaction audit;
- signing/update compromise drill;
- multi-tenant authorization suite;
- secure restore and incident tabletop;
- OWASP ASVS/CASA-oriented control mapping.

## 26. Acceptance criteria

1. Every trust boundary has authenticated/authorized/validated contract.
2. Canary secrets never reach any log/trace/support bundle.
3. Public UI/API contains no Roblox API-key flow.
4. Cross-tenant/IDOR/property tests pass.
5. Malicious repository/native payload corpus cannot execute arbitrary host/Studio code.
6. Update signature/rollback attacks fail closed.
7. Wrong-target/approval-replay/lost-response release attacks fail safely.
8. External security review critical/high findings resolved before public beta.

