# SPEC-03 — Project Manifest and Configuration

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02

## 1. Configuration files

NeuMan recognizes these files:

| File | Authorship | Committed? | Purpose |
|---|---|---:|---|
| `neuman.project.yaml` | Human | Yes | Project topology, ownership, environments, policies |
| `neuman.lock.json` | Machine | Yes | Exact toolchain/schema/dependency resolution |
| `.neuman/local.json` | Machine/user | No | Local workspace IDs, UI preferences, cached selections |
| `.neuman/releases/*.json` | Machine | Yes or mirrored | Signed release manifests/receipts when Git ledger enabled |
| `.neuman/art/*.json` | Machine | Yes | Art channel pointer/metadata; native blobs use provider |
| `.neuman/ignore` | Human | Yes | NeuMan-specific capture/index exclusions |

Secrets MUST NOT appear in any of these files.

## 2. Discovery

The daemon searches the selected directory and ancestors for `neuman.project.yaml`, stopping at filesystem root or repository boundary. Nested project manifests are allowed only when the nested manifest declares `allowNested: true` and uses a disjoint Git worktree root. Ambiguous discovery is an error.

Symlinks are resolved before trust and ownership checks. A manifest outside the selected repository requires explicit user approval.

## 3. Root schema

Normative root fields:

```yaml
schemaVersion: "1.0"
project:
  slug: example-game
  displayName: Example Game
  allowNested: false
repository: {}
toolchain: {}
providers: {}
artChannels: {}
places: {}
environments: {}
policies: {}
validation: {}
extensions: {}
```

Required: `schemaVersion`, `project`, `repository`, `toolchain`, `providers`, `places`, `environments`, `policies`.

Unknown root keys are invalid except beneath `extensions`. Extension keys use reverse-DNS names, for example `extensions.com.example.pipeline`.

## 4. Project section

```yaml
project:
  slug: example-game
  displayName: Example Game
  description: Optional text
  allowNested: false
  defaultPlace: lobby
  defaultArtChannel: art-main
```

Rules:

- `slug` matches `[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])?`.
- `displayName` is 1–100 Unicode scalar values.
- referenced defaults MUST exist.
- slug changes require explicit migration because Hub and paths may use it.

## 5. Repository section

```yaml
repository:
  provider: github
  remote: https://github.com/example/game.git
  githubRepositoryId: "123456789"
  defaultBranch: main
  releaseBranch: main
  objectFormat: sha1
  projectFile: default.project.json
  requireCleanWorktreeForBuild: true
  allowSubmodules: false
  allowGitHooks: false
```

Rules:

- credentials in `remote` are invalid;
- repository ID, if provided, is authoritative for GitHub API binding;
- submodules are disabled by default and require each URL/commit to pass policy;
- Git hooks are never run by NeuMan unless a trusted-project policy explicitly opts in;
- build inputs use commit OIDs, never branch names alone.

## 6. Toolchain section

```yaml
toolchain:
  neuman: ">=0.1.0 <0.2.0"
  studio:
    channel: production
    buildConstraint: observed-and-approved
  rojo:
    version: 7.7.0
    source: bundled
  apiSchema:
    policy: exact-lock
  helpers:
    stylua: { version: 2.1.0, required: false }
    selene: { version: 0.29.0, required: false }
```

The project file expresses constraints. `neuman.lock.json` stores exact versions and checksums. A build fails if constraints and lock disagree.

## 7. Provider section

```yaml
providers:
  artStore:
    type: git-lfs
    options:
      pathPrefix: .neuman/blobs
  hub:
    type: neuman-hub
    url: https://hub.example.com
    projectId: prj_...
  gitHub:
    appSlug: neuman-example
  roblox:
    oauthAppId: public-client-id
```

Provider options MUST contain no credentials. Provider-specific schemas are versioned and namespaced.

Supported art-store types: `local-cas`, `git-lfs`, `s3`, `neuman-hub`, and experimental `lore`. A project MAY use a primary and read-through mirrors, but one provider is authoritative for writes at a time.

## 8. Art channels

```yaml
artChannels:
  art-main:
    displayName: Main Art
    protected: true
    authoringPlace: authoring-lobby
    acceptancePolicy: art-review
    lockPolicy: enforced
    allowOfflineCapture: true
    allowOfflineAcceptance: false
```

An authoring place may feed more than one channel only if the plugin requires explicit active-channel selection and the cell sets are disjoint. Protected channels require Hub or an operator-defined equivalent approval ledger.

## 9. Environments

```yaml
environments:
  authoring:
    kind: authoring
    productionImpact: false
  staging:
    kind: staging
    productionImpact: false
    requiredFor: [production]
  production:
    kind: production
    productionImpact: true
    approvals: production-approval
```

Environment keys match `[a-z][a-z0-9-]{1,31}`. `productionImpact: true` activates high-impact security rules regardless of the environment name.

## 10. Place schema

```yaml
places:
  lobby:
    displayName: Lobby
    baseTemplate:
      type: repository-file
      path: places/lobby.base.rbxl
      sha256: sha256:...
    authoring:
      universeId: "111"
      placeId: "222"
      creator: { type: group, id: "333" }
    targets:
      staging:
        universeId: "444"
        placeId: "555"
        creator: { type: group, id: "333" }
        publication: studio-assisted
      production:
        universeId: "666"
        placeId: "777"
        creator: { type: group, id: "333" }
        publication: studio-assisted
    ownership: []
    terrain: {}
    serviceState: {}
    validationProfile: default
    releasePolicy: standard
```

Target tuple `(universeId, placeId)` MUST be globally unique within the project unless explicitly declared an alias. A production-impact target cannot also be an authoring target.

## 11. DataModel path syntax

Paths are absolute and start with `/`. Segments use JSON Pointer escaping:

- `~0` represents `~`;
- `~1` represents `/`.

Example: an instance named `UI/Menu~Old` under `ReplicatedStorage` is `/ReplicatedStorage/UI~1Menu~0Old`.

Paths identify declared slots, not durable instance identity. Art cell identity uses CellId.

## 12. Ownership entries

```yaml
ownership:
  - id: server-code
    path: /ServerScriptService
    owner: git-code
    projectPath: src/server
    unknownInstances: reject
  - id: world-art
    path: /Workspace/Art
    owner: studio-art
    channel: art-main
    cells:
      strategy: children
      requiredAttribute: NeuManCellId
  - id: terrain
    path: /Workspace/Terrain
    owner: terrain
    channel: art-main
  - id: generated
    path: /ReplicatedStorage/NeuManGenerated
    owner: generated
```

Owner-specific required fields:

- `git-code`: `projectPath` or Rojo project mapping.
- `studio-art`: `channel` and `cells`.
- `terrain`: `channel` and terrain policy.
- `service-state`: service schema reference.
- `external-package`: package policy.
- `generated`: generator identifier.

Overlap validation resolves service/root aliases and all include/exclude rules. Parent and child ownership MAY differ only when the parent explicitly sets `allowOwnedDescendants: true` and does not claim unknown descendants.

For the desktop's strict v1 Rojo preflight, every filesystem-backed `git-code` root MUST declare `projectPath`. A mapped source must canonicalize beneath it. An ownership root MAY set `allowRojoBinaryModels: true` to acknowledge that `.rbxm`/`.rbxmx` contents cannot be structurally proven by the static project parser. The option applies only inside that exact `git-code` root and does not relax DataModel overlap, symlink, path-containment, include, or Studio-art separation checks.

## 13. Cell policy

```yaml
cells:
  strategy: children
  idAttribute: NeuManCellId
  warnSizeBytes: 20971520
  maxSizeBytes: 100663296
  scriptPolicy: reject
  externalReferences: explicit
  requireLock: true
  autoCheckpointIdleMs: 0
```

`autoCheckpointIdleMs: 0` disables automatic accepted checkpoints; the plugin may still capture local drafts. Hard size cannot exceed configured transport/storage limits.

## 14. Terrain policy

```yaml
terrain:
  enabled: true
  channel: art-main
  resolutionStuds: 4
  tileSizeStuds: [512, 256, 512]
  originStuds: [0, -256, 0]
  overlapStuds: 0
  encoding: phase0-selected
  requireLock: true
```

Tile sizes MUST align to Roblox terrain resolution. Overlap is zero in v1; nonzero overlap requires deterministic conflict rules not currently specified.

## 15. Service-state policy

```yaml
serviceState:
  Lighting:
    owner: studio-art
    channel: art-main
    properties: [Ambient, Brightness, ClockTime, EnvironmentDiffuseScale]
    children: native-cells
    requireLock: true
```

Only explicitly listed readable/writable properties are captured. Unknown, hidden, or not-scriptable properties are reported and handled by compatibility policy.

## 16. Policy definitions

```yaml
policies:
  art-review:
    type: art-acceptance
    approvals: 1
    requireDifferentApprover: false
    requiredChecks: [native-roundtrip, no-scripts, references-resolved]
  production-approval:
    type: release-approval
    approvals: 2
    requireDifferentApprover: true
    roles: [production-approver]
  standard:
    type: release
    requireCleanCodeCommit: true
    requireAcceptedArtRevision: true
    requireNoUnknownDrift: true
    requireEnvironmentProof: staging
    rollbackRequired: true
```

Policy resolution snapshots the exact canonical policy into build/release records. Later policy edits do not rewrite history.

## 17. Validation configuration

```yaml
validation:
  profiles:
    default:
      forbiddenClassesInArt: [Script, LocalScript, ModuleScript]
      forbidRequireAssetId: true
      maxPlaceBytes: 104857600
      unresolvedReference: error
      unknownProperty: error
      externalAssetPermission: error
      lint:
        stylua: check
        selene: required
```

Roblox platform limits are repeated in validation only for early feedback; the external platform remains authoritative.

## 18. Lockfile

`neuman.lock.json` contains:

```json
{
  "schemaVersion": "1.0",
  "generatedAt": "2026-07-09T22:14:03.127Z",
  "manifestHash": "b3-256:...",
  "toolchain": {
    "neuman": {"version":"0.1.0","sha256":"sha256:..."},
    "rojo": {
      "version":"7.7.0",
      "artifacts": {
        "x86_64-pc-windows-msvc": {
          "path":"rojo/7.7.0/x86_64-pc-windows-msvc/rojo.exe",
          "sha256":"sha256:..."
        },
        "aarch64-apple-darwin": {
          "path":"rojo/7.7.0/aarch64-apple-darwin/rojo",
          "sha256":"sha256:..."
        }
      }
    },
    "studio": {"channel":"production","observedBuild":"..."},
    "apiSchemaHash":"b3-256:..."
  },
  "providers": {},
  "dependencies": {},
  "resolutionHash": "b3-256:..."
}
```

Generation uses canonical sorted output. Manual lockfile edits are detected by hash/checksum validation and rejected.

`toolchain.rojo.artifacts` is keyed by qualified target triple. Artifact paths are normalized relative paths under the Rust-selected tool root; they cannot be supplied by the webview, be absolute, traverse, or resolve through a symlink outside that root. Before every new live session or build, NeuMan verifies the selected file's exact lowercase SHA-256 and exact `rojo --version` response. The lockfile `manifestHash` MUST equal the validated project manifest hash. An installation MAY use an application-managed tool root outside the workspace, but that root is native policy rather than manifest/webview input.

## 19. Local configuration

`.neuman/local.json` may store:

- workspace ID;
- last selected place/art revision;
- window layout and non-sensitive preferences;
- local Rojo/Studio session metadata;
- cache paths;
- paired plugin installation IDs, but not raw session secrets;
- opt-in telemetry choice.

It MUST be added to `.gitignore`. Credentials are stored by opaque keychain reference only.

## 20. Environment overrides

Environment variables are allowed only for deployment/runtime endpoints and keychain references, using prefix `NEUMAN_`. They MUST NOT alter ownership, release policy, or target production IDs without appearing in the effective-configuration preview and audit evidence.

Precedence, lowest to highest:

1. built-in safe defaults;
2. project manifest;
3. committed environment block;
4. local non-security preferences;
5. explicit CLI flag for that operation.

Security and production policy can only become stricter through local/CLI override unless an authorized manifest change is committed.

## 21. Parsing and validation

- YAML aliases and anchors are rejected in v1 to avoid hidden configuration expansion.
- Duplicate keys are errors.
- Maximum manifest size is 1 MiB.
- Maximum nesting depth is 64.
- Strings are normalized to Unicode NFC for comparison but original display text is retained.
- Absolute host paths are forbidden in committed configuration except explicitly marked developer-tool hints.
- URL schemes are allowlisted per field.
- Validation reports all independent errors in one pass where safe.

## 22. Migration

The CLI command `neuman config migrate --to <version>`:

1. parses without mutation;
2. writes a sibling backup;
3. produces a semantic diff;
4. requires confirmation unless `--check`;
5. writes atomically;
6. updates the lockfile only after successful validation.

Automatic destructive migration is prohibited. Older major versions remain readable for the documented support window.

## 23. Acceptance criteria

1. JSON Schema or equivalent machine schema exists for YAML after parsing.
2. Golden valid/invalid fixtures cover every field and owner type.
3. Duplicate ownership and target identity are rejected.
4. Secrets and credential-bearing URLs are rejected.
5. Effective configuration output is stable and canonical.
6. Manifest and lockfile hashes reproduce on Windows and macOS.
7. Migration is atomic and round-trip tested.
8. Fuzzed YAML cannot cause unbounded memory, recursion, or code execution.
