# NeuMan core vertical slice

This implementation turns the durable specifications into a production-shaped local vertical slice. It intentionally does not claim that Rojo or Roblox Studio assembly is complete: the build API accepts an already assembled candidate place and gives that artifact deterministic identity, validation, storage, and release semantics. The Studio bridge and desktop layers can call the same public core APIs.

## Files

- `domain.rs` contains canonical IDs, BLAKE3 content hashes, canonical JSON, immutable art/build/bundle records, and release state transitions.
- `core.rs` contains manifest parsing, local SQLite, CAS, art capture, Git validation, deterministic build composition, OAuth PKCE, the guarded Roblox publisher, release records, and fail-closed preflight.
- `neuman_cli.rs` is the asynchronous CLI and versioned JSON adapter.

## Security properties

- Roblox IDs remain decimal strings in durable JSON.
- Domain IDs validate their UUID version and canonical lowercase form.
- Artifact identity is BLAKE3-256 using lowercase, unpadded base32.
- Canonical identities use RFC 8785/JCS (`serde_jcs` 0.2) plus explicit versioned domain separators. Golden tests cover ECMAScript float formatting, negative zero, control/string escaping, and UTF-16 property order.
- Manifest provider blocks reject credential-like keys and credential-bearing Git URLs.
- YAML anchors and aliases are rejected before parsing.
- Ownership roots are normalized and overlapping roots are rejected unless the parent explicitly delegates descendants and does not capture unknown children.
- CAS writes use a create-new temporary file, flush, atomic rename, and read-after-write hash verification.
- Art revision IDs cannot be reused with different metadata.
- Git is invoked as an argument array with hooks disabled; builds use exact commit OIDs and can require a clean worktree.
- OAuth uses authorization code flow with S256 PKCE, 256-bit verifier/state values, an official fixed authorization origin, exact loopback redirect matching, and state validation.
- The public OAuth path never calls the API-key-only Place Publishing endpoint.
- Place Publishing accepts only an `OperatorApiKey`; cookie-like strings, whitespace, and header injection are rejected. The production origin is fixed to `https://apis.roblox.com` and redirects are disabled.
- The CLI accepts that operator key only through a named environment variable and never serializes it.
- Release preflight treats unknown drift, missing staging proof, missing lease, permission uncertainty, artifact corruption, and predecessor mismatch as blocking.
- Provider ambiguity becomes `unknown-external-state`, never success or a blind retry.

## CLI

All commands accept global `--root` and `--json`. In JSON mode stdout contains exactly one envelope:

```json
{"schemaVersion":"1.0","ok":true,"result":{},"error":null}
```

Supported vertical-slice commands:

```text
neuman init --slug <slug> --name <name> [--force]
neuman validate
neuman project validate
neuman status
neuman code probe
neuman code status
neuman code fetch --remote origin [--prune] [--tags auto|all|none]
neuman code sync --remote origin [--upstream origin/main]  # fetch + ff-only update
neuman code update origin/main                             # already-fetched ff-only update
neuman bridge status
neuman art status
neuman art show <art_revision_id>
neuman art capture --cell /Workspace/Art/Tree=tree.rbxm --message "Tree pass" [--accept]
neuman build create --art <art_revision_id> [--place lobby] [--code <oid>] [--candidate place.rbxl]
neuman release create --bundle <hash> --environment staging [--place lobby] [--approved]
neuman release plan ...             # alias of create; creates the immutable plan/record
neuman release preflight <release_id> --permission-verified --predecessor-matches --drift clean --lease-held --staging-proof-valid
neuman release publish <release_id> --permission-verified --predecessor-matches --drift clean --lease-held --staging-proof-valid
neuman release status <release_id>
```

`release publish` reads `NEUMAN_ROBLOX_OPERATOR_API_KEY` by default. Operators can select another variable name with `--operator-key-env`; there is deliberately no `--api-key` flag.

## Local data

The initialized project stores:

```text
.neuman/project-id             stable local ProjectId
.neuman/state.sqlite3          metadata ledger (WAL)
.neuman/cas/objects/b3-256/    immutable native/artifact objects
.neuman/cas/tmp/               atomic write staging
```

SQLite tables are append-only for art revisions, bundles, and release receipts. Release status changes use expected-state comparison. CAS objects are never trusted without rehashing.

## What remains for the adjacent systems

- The Studio bridge supplies authenticated native RBXM bytes and stable cell IDs. The CLI import path generates cell IDs only as a local bootstrap convenience.
- `git_rojo.rs` performs qualified Git fetch/ff-only update, isolated worktrees, pinned Rojo verification/build planning, and loopback supervision. The remaining Studio runner inserts native art into Rojo output before passing the candidate bytes and exact resolved inputs to `create_build_bundle`.
- The Hub replaces local approval/lease assertions with authoritative principals, policies, and expiring leases.
- Provider qualification must confirm the current Roblox response schema and unsupported-place-instance scanner before enabling Open Cloud publishing for a project.
- The Tauri desktop now performs OAuth token exchange, signed-ID-token/userinfo validation, OS credential-vault storage, and revocation. Core keeps the reusable PKCE/state primitives and does not persist tokens.

## Verification

Inline tests cover canonical ID/hash round trips, deterministic JSON/build identities, state-machine rejection, YAML alias/secret/ownership failures, CAS corruption, immutable revision writes, SQLite build/bundle round trips, PKCE state confusion, unknown drift, cookie rejection, and a mocked HTTP Place Publishing request/receipt.
