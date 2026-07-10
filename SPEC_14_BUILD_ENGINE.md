# SPEC-14 — Build Engine

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-03, SPEC-09–13

## 1. Purpose

The Build Engine resolves immutable code, art, dependency, policy, base-template, and toolchain inputs into a validated logical build and immutable release bundle. It produces evidence sufficient to reproduce or explain the build and never publishes directly as an implicit side effect.

## 2. Reproducibility definition

NeuMan distinguishes:

- **Input reproducibility** — identical canonical inputs and policy produce the same LogicalBuildHash.
- **Artifact byte reproducibility** — repeated assembly produces byte-identical artifact hashes.
- **Semantic reproducibility** — repeated engine assembly produces equivalent managed DataModel state even if serializer bytes differ.

Input reproducibility is REQUIRED. Artifact byte reproducibility is REQUIRED for pure Rojo/Open Cloud artifacts if the toolchain proves it. Studio-assisted assembly MUST report exact artifact or semantic verification level; it MUST NOT claim byte reproducibility without evidence.

Staging and production promote the same immutable ReleaseBundle. They do not resolve branches, art channel heads, mutable package versions, or tool versions again.

## 3. Build request

Required:

- project ID and manifest/lockfile commit/hash;
- place key;
- exact code commit OID and repository identity;
- exact accepted art revision ID/state root;
- exact base template content hash;
- exact dependency manifest hash;
- exact toolchain lock hash;
- exact policy revision hash;
- requester and reason;
- requested validation/test profile.

Branch names, `latest`, channel heads, or unversioned local files are invalid final build inputs. UI may resolve them before request and show resulting immutable values.

## 4. LogicalBuildHash v1

Canonical JCS JSON:

```json
{
  "schemaVersion": "1.0",
  "projectId": "prj_...",
  "placeKey": "lobby",
  "repository": {"id":"...","objectFormat":"sha1"},
  "codeCommit": "...",
  "artRevisionId": "art_...",
  "artStateRootHash": "b3-256:...",
  "baseTemplateHash": "b3-256:...",
  "dependencyManifestHash": "b3-256:...",
  "manifestHash": "b3-256:...",
  "toolchainLockHash": "b3-256:...",
  "policyRevisionHash": "b3-256:...",
  "profile": "release"
}
```

LogicalBuildHash is BLAKE3-256 of UTF-8 canonical bytes with domain prefix `neuman-logical-build-v1\0`. Requester/time/message are excluded; they belong to Build entity, not identity.

## 5. Build plan DAG

Reference nodes:

1. `resolve-config`
2. `resolve-code`
3. `resolve-art`
4. `resolve-dependencies`
5. `verify-toolchain`
6. `materialize-code-worktree`
7. `materialize-art`
8. `static-code-validation`
9. `art-validation`
10. `dependency-validation`
11. `ownership-validation`
12. `rojo-build-base`
13. `assemble-native-content`
14. `engine-validation`
15. `tests`
16. `package-release-bundle`
17. `sign-provenance`

Independent read-only validation nodes run in parallel. Nodes declare immutable input hashes and output hashes so cache reuse is safe.

## 6. Resolution

### Code

- fetch presence of exact commit as policy allows;
- verify commit belongs to configured repository;
- verify signatures/branch ancestry if policy requires;
- check commit reachable from approved ref/PR merge base;
- reject shallow/partial missing objects unless fetched safely;
- materialize clean detached worktree.

### Art

- require accepted/specified status;
- verify state root and revision signature/approval;
- fetch every referenced native/terrain/service/dependency object;
- verify hashes and compatibility metadata;
- reject missing/garbage-collected objects.

### Base template

- exact content hash from repository/provider;
- class/place format valid;
- no unexpected scripts/content outside declared ownership;
- template provenance recorded.

### Dependencies

- resolve already pinned asset/package/tool entries;
- no mutation of accepted art revision dependency meaning;
- recheck external availability/permission as validation evidence, not silently replace IDs/versions.

## 7. Toolchain verification

- exact NeuMan/runner/plugin/Rojo/helper versions/checksums;
- Studio observed build satisfies approved compatibility record;
- API schema hash exact or approved migration;
- Git/LFS/provider capabilities;
- OS/platform support;
- code signatures where applicable.

Downloaded tool binaries are accepted only from signed/allowlisted sources with checksum. Project-supplied arbitrary tools are denied unless trusted extension specification exists.

## 8. Materialization root

Each attempt uses a new operation directory:

```text
builds/<BuildId>/attempt-<n>/
  inputs/
  worktree/
  art/
  candidate/
  output/
  logs/
  receipts/
  temp/
```

Permissions restrict current OS user. Paths are canonicalized; no input may escape via symlink/junction. Materialization never occurs in developer source tree.

## 9. Code validation

Baseline:

- Git worktree clean at exact commit;
- manifest/lock consistent;
- Rojo project parse/ownership valid;
- source map generation succeeds;
- Luau parse/type/static checks configured;
- StyLua check and Selene where required;
- no merge conflict markers;
- no forbidden require-by-asset-ID/dynamic loader patterns per security policy;
- dependencies/license/provenance policy;
- secrets scan on changed/full scope per policy;
- no prohibited generated or executable file.

Validators are exact-version pinned and results include tool/config/input hash.

## 10. Art validation

For complete art state:

- state root reproduces;
- every object hash verifies;
- cell identity/ownership/nesting valid;
- scripts/forbidden classes absent;
- external refs resolve;
- asset/package dependencies match extracted manifests;
- terrain tiles align/cover expected dirty set;
- service property adapters compatible;
- index completeness meets policy;
- native snapshots compatible/migrated;
- size/complexity budgets;
- no duplicate cell/instance IDs;
- no unapproved opaque changes.

## 11. Dependency validation

- exact asset/package IDs/versions;
- target-environment permission evidence freshness;
- availability/moderation status;
- source provenance/licenses if configured;
- no cyclic package/dependency issue;
- mutable external dependency reported and allowed;
- tool/download checksums.

Target-specific permission checks may be deferred to release preflight, but build receipt states `target-neutral-unverified` rather than success for that dimension.

## 12. Ownership validation

Resolve final candidate logical tree and require:

- each instance path one owner;
- Git code only from worktree;
- art cells only from selected art state;
- generated content only from versioned generator;
- external package slots pinned;
- no unknown root with destructive ambiguity;
- no Rojo mapping overwrites native art slots.

## 13. Base assembly profiles

### 13.1 Pure Rojo artifact

Use when all content/property classes are supported and publication scanner approves. Rojo builds `.rbxl` or `.rbxlx`; output hash recorded. Studio engine validation still SHOULD load it before release.

### 13.2 Studio-native assembly

1. Rojo builds code/declarative base and art slots.
2. Studio opens the local candidate with the signed NeuMan plugin active.
3. The plugin consumes the fixed signed runner manifest and inserts exact native cells, terrain, and service state through plugin-authorized engine APIs.
4. Cross-cell references and package policies resolve.
5. Engine validators/tests run.
6. Runner emits assembly receipt.

Operator-owned CI MAY instead use the stable Open Cloud Luau Execution profile with binary inputs and the same fixed runner manifest/receipt semantics. CLI `RunScript` remains useful for supported command-bar validation and launch supervision, but it is not assumed to provide `SerializationService`'s `PluginOrOpenCloud` capability.

If Studio cannot export final local place bytes through a supported path, ReleaseBundle contains base candidate plus exact native inputs and runner manifest; staging/production use the same bundle to assemble logically equivalent targets. This limitation is explicit.

## 14. Runner operation manifest

Signed/canonical fields:

- executor profile `studio-plugin` or `operator-open-cloud`;
- fixed runner implementation SHA-256;
- operation type `assemble-validate`;
- BuildId/LogicalBuildHash;
- project/place binding;
- exact universe/place/source-place-version binding for operator Open Cloud;
- expiry/nonce;
- candidate base hash;
- ordered cells/terrain/services with hashes/slots;
- expected ownership/config/schema;
- validator/test IDs;
- forbidden content policy;
- profile-compatible receipt channel: authenticated numeric loopback for Studio plugin or signed task return correlation for Open Cloud.

No code body or arbitrary method arguments.

Profile dispatch is immutable after signing. Persisted Open Cloud jobs/actions contain no API key; only the operator-owned HTTP adapter receives that credential out of band. Upload/task-creation dispatch is durably recorded before execution, so crash recovery or an unknown mutation response requires reconciliation rather than emitting a blind duplicate. Read-only task polls may be retried.

## 15. Engine validation

Inside Studio:

- all expected roots exist once;
- cell IDs/parents/classes match;
- refs resolve and class constraints pass;
- terrain/service readback signatures;
- Script sources map to expected code files/source map;
- no unexpected scripts/backdoors/capabilities;
- no forbidden external assets/packages;
- place size estimate/platform save readiness;
- engine can traverse/clone/serialize configured fixtures;
- configured static TestEZ/validation modules run only if they are pinned trusted build code and invoked through fixed harness.

Runner reports each validator version/result/hash.

## 16. Tests

Levels:

- pure Rust unit/contract tests;
- static code/art validators;
- Studio edit-mode engine validation;
- configured Luau unit tests in controlled Studio runner;
- staging publish smoke tests occur in release, not local build;
- performance budget checks optional build profile.

Test failure never produces release-eligible bundle unless policy explicitly marks test non-blocking and receipt records it.

## 17. Output artifacts

Possible artifacts:

- `.rbxl/.rbxlx` candidate/final artifact;
- Studio-native assembly bundle;
- source map;
- art state manifest;
- dependency manifest;
- validation/test report;
- logical build manifest;
- provenance statement;
- SBOM/license report;
- checksums/signatures;
- logs (redacted, not required to execute bundle).

Every artifact has media type, size, BLAKE3, SHA-256 for published files, producer step/version, and retention class.

## 18. ReleaseBundle manifest

Contains only immutable references and bytes needed for promotion:

- schema/version;
- logical build hash;
- target constraints/place key but no credentials;
- artifact set/hash/size/media type;
- runner manifest template and exact runner hash;
- validation/test receipts;
- toolchain/provenance;
- publication method compatibility report;
- rollback metadata prerequisites;
- signatures.

Bundle hash uses canonical JCS manifest plus ordered artifact hashes with domain separation. Bundle is immutable.

## 19. Cache

Cache key = step implementation version + canonical input hashes + platform/toolchain identity where relevant.

Never cache:

- auth/permission outcome beyond its evidence expiry;
- current external moderation/availability indefinitely;
- mutable branch/channel resolution;
- failed/partial output as successful;
- Studio result across unapproved Studio build change.

Cache hit verifies output object hash and provenance before use.

## 20. Build attempts and retries

Same BuildId can have multiple attempts only with identical LogicalBuildHash/profile. Each attempt has unique operation ID/logs. Transient provider/process failures may retry. Deterministic validation failure requires changed input/config/new build or explicit rerun for diagnostics.

Cancellation before external/Studio commit cleans temporary state and retains receipts. Killing Studio runner requires safe process ownership and may yield interrupted/unknown; reconciliation follows SPEC-06/13.

## 21. Security

- Clean worktree and untrusted input path controls.
- No shell/hook/arbitrary runner execution.
- Native models scanned before engine insertion and again in engine.
- Build network access default denied except declared providers; full sandbox profile is target architecture.
- Secrets absent from bundle/artifacts/logs.
- Provenance signed by local key or Hub builder identity.
- Dependency confusion and symlink/path traversal tests.

## 22. Provenance

Provenance records:

- builder identity/version/platform;
- invocation ID/time;
- exact inputs;
- materials/artifacts;
- tool checksums;
- parameters/profile;
- test/validation summaries;
- whether build was isolated and network policy;
- signatures.

Format SHOULD align with in-toto/SLSA concepts while retaining NeuMan-specific fields.

## 23. Error codes

- `BLD_INPUT_MUTABLE`
- `BLD_INPUT_MISSING`
- `BLD_LOGICAL_HASH_MISMATCH`
- `BLD_TOOLCHAIN_MISMATCH`
- `BLD_MATERIALIZATION_FAILED`
- `BLD_VALIDATION_FAILED`
- `BLD_OWNERSHIP_FAILED`
- `BLD_ASSET_PROOF_EXPIRED`
- `BLD_ROJO_FAILED`
- `BLD_STUDIO_ASSEMBLY_FAILED`
- `BLD_ENGINE_VALIDATION_FAILED`
- `BLD_TEST_FAILED`
- `BLD_ARTIFACT_CORRUPT`
- `BLD_BUNDLE_SIGNING_FAILED`
- `BLD_CANCELLED`

## 24. Acceptance criteria

1. LogicalBuildHash golden vectors match independent implementations.
2. Branch/channel changes after request cannot change build inputs.
3. Build never reads untracked/dirty developer files.
4. Cache poisoning/corruption is detected.
5. Studio and pure build profiles report exact reproducibility level.
6. ReleaseBundle fully identifies promotion inputs without secrets.
7. Failure injection at every DAG node leaves no falsely successful artifact.
8. Same bundle can be verified from clean machine with provider access.
