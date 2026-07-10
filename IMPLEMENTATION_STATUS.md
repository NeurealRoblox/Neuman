# NeuMan implementation and qualification status

Status date: 2026-07-10  
Code version: 0.1.0 development reference

This document distinguishes implemented behavior from live-provider and Roblox Studio qualification. “Implemented” means executable code present in this checkout with local tests. It does not mean a public production release has passed every Phase-0 acceptance gate in SPEC-22.

## System matrix

| System | Implemented now | Verification in this checkout | Still required before production |
|---|---|---|---|
| Durable specifications | Architecture plus SPEC-00 through SPEC-22 | Cross-linked Markdown set | Ratify v1 schemas and compatibility policy |
| Domain and identity | Typed UUIDv7/v4 IDs, decimal Roblox IDs, Git OIDs, RFC 8785/JCS, BLAKE3 base32, SHA-256, ArtState Merkle, build/release state | Golden vectors and state-machine tests | Freeze public schema fixtures across languages |
| Project manifest | Bounded YAML, alias/credential rejection, path/provider validation, full ownership overlap rules, target/policy validation, and explicit Rojo binary-model opt-in | Multi-error validation and hostile ownership fixtures | JSON Schema publication and migration fixtures |
| Local ledger and CAS | SQLite WAL records, immutable art/build/bundle data, durable Studio transfer/capture receipts, accepted art-head compare-and-swap, expected-state release updates, atomic verified CAS | Corruption, Studio capture idempotency/ownership/head-overlay, immutability, round-trip tests | Backup/restore CLI, GC root traversal, crash/fault-injection matrix |
| Git code lane | Probe, exact status, credential-free remote allowlist, hostile repo-config rejection, hooks/filters/ext/helper shutdown, fetch by configured remote name, incoming-commit filter scan, explicit fast-forward only, exact commit validation | Real repositories/bare remotes plus malicious credential-helper and checkout-filter fixtures | Qualified safe-directory UX, LFS provider, commit signing/case/long-path policy |
| Rojo | Checksum/version-sealed executable capability, isolated worktrees, managed per-workspace/place sessions, and strict recursive `.project.json`/`$path` ownership preflight that rejects escape, symlinks, dynamic constructs, destructive cross-owner mappings, and unapproved binary models | Session tests plus 11 hostile ownership/include/path fixtures, desktop-feature compile/Clippy, and renderer interaction test | Real pinned Rojo/Studio compatibility and protocol-freshness matrix; LFS materialization |
| Native build identity and runner contract | Exact logical input/bundle identity; signed profile-specific manifests/receipts; fixed runner identity; Studio-loopback and operator-Open-Cloud task evidence; key-free serializable jobs; crash-safe operator action state machine with reconciliation-required ambiguous mutation state | Tamper/profile/source/task/publish evidence, key-isolation, replay, crash recovery, and state-machine tests | Implement the concrete fixed Luau runner and provider adapter, wire Git+accepted-art assembly without a preassembled candidate, and qualify both selected execution profiles over the Studio golden corpus/`SavePlaceAsync` |
| Roblox OAuth | Public-client PKCE, fixed registered loopback, exact no-redirect bounded initial/refresh transports, OIDC/JWKS/userinfo correlation, fixed scopes, validated fail-closed OS-vault restore, mandatory refresh rotation, and transparent unconfirmed-revocation warning | PKCE/state, hostile-origin/endpoint/body/scope/rotation/identity/JWKS/redaction, vault schema/clock/failure, and desktop tests | Real approved Roblox client; consent, initial/refresh/revocation/account-switch qualification; OS credential-backend matrix |
| Roblox resources | Vault-only native read provider for token-resource grants, owner-wide exact probes, stable OAuth universe/place reads, bounded place discovery, typed selection evidence, and a renderer metadata view with no mutation surface | Six deterministic mock-provider tests, desktop compile, TypeScript build, and browser interaction/console test | Live public-client token-resource/universe/place qualification and current provider pagination/throttling evidence |
| Roblox publication | Operator-only API-key type, fixed `apis.roblox.com` origin, redirects disabled, multipart publish, receipt/error classification; no desktop publish command | Mock HTTP request/receipt tests | Replace CLI-supplied permission/predecessor/drift/staging assertions with fresh provider/Hub evidence, add unsupported-instance scan, and run disposable-universe publish/rollback/drift drills |
| Studio bridge | Numeric loopback discovery/WS, explicit pairing, scoped credentials, replay protection, exact workspace/project/place/channel generation filtering, context/resource-bound transfers, CAS/ledger-before-ACK capture, explicit accepted base/stale rejection, source-excluding fan-out, and authenticated 96 MiB downloads | Cross-place/workspace non-disclosure, transfer-switch race, stale-base, accepted fan-out, real loopback download, and BLAKE3 tests | Persist paired credential across restart and qualify OS firewall/Studio WebStream/large-download behavior in supported Studio builds |
| Studio plugin | Widget, pairing, registration, dirty epochs, native serialization, context-bound chunk upload, accepted-base advancement, incoming preview, authenticated size/hash-checked download, staged dirty-safe atomic apply/rollback, and explicit rejection of an unqueued second apply | Lockfile-pinned StyLua 2.5.2 Luau parse/format gate plus protocol/hash tests | Windows/macOS golden corpus, post-apply reserialization/state-root proof, terrain/service adapters, reference rewriting |
| Desktop | Typed native allowlist, embedded bridge, exact pairing, Git/Rojo controls, vault-only OAuth resource inventory, durable local capture/fan-out, and native optional-Hub polling/import/fan-out with no renderer bearer path | 112 library + 2 CLI + 5 desktop tests, default/desktop Clippy with warnings denied, clean npm/Luau/Vite build, and browser interaction with no console errors | Signed installer/update rehearsal, accessibility audit with assistive tech, and live provider/Studio qualification |
| Optional self-hosted Hub | User-operated auth/storage; canonical full-state cell manifests; server-recomputed roots/changed resources/leases; proposal/head CAS; CAS transfers; durable events/WS; native desktop cursor/import adapter; presence/build/release/audit/quotas | Canonical tamper/omission, lease race, head CAS, durable cursor/import, transfer-origin, tenant non-disclosure, and router tests | PostgreSQL/S3 adapters, real OIDC/JWKS, HA/backup/restore/load/chaos and cross-machine recovery qualification |
| GitHub App boundary | Webhook HMAC before parse, replay store, RS256 App JWT, scoped installation token, numeric repository identity, checks, markers | Eight mocked/provider-independent tests | Wire routes/workers to Hub DB/outbox/secret manager; live App permissions, webhook, ruleset, rate-limit drills |
| Open-source release engineering | Apache-2.0 metadata, contribution/security/conduct/governance/DCO/CODEOWNERS/changelog docs, pinned toolchains/dependencies, signed-tag/native-signing/notarization/updater/checksum/attestation contracts | Release fixtures, JSON/YAML/Node parsing, Rust/frontend/Luau gates; preflight fails intentionally and only on the absent official updater public key | Materialize root workflow sources under `.github/workflows`, ratify CODEOWNERS, configure protected environments/certificates/OAuth ID/updater key, add qualified SBOM, and run a signed update rehearsal |

## End-to-end flows currently runnable

### Local deterministic flow

1. Initialize and validate a project.
2. Inspect or fast-forward-sync a configured Git remote.
3. Capture project-relative RBXM cells into immutable proposed/local accepted art revisions according to channel policy.
4. Create an immutable logical build and bundle from exact Git/art/base/policy/toolchain inputs and an explicitly supplied assembled candidate.
5. Create, preflight, and inspect a release record.
6. Exercise the guarded operator-key publication reference only with externally verified evidence. The current CLI assertion inputs are not production authorization evidence.

### Shared Hub flow

1. Start `neuman-hub` with long random development secrets.
2. Create a project and persisted role membership.
3. Upload and verify scoped CAS objects and a canonical full-state cell manifest.
4. Let the Hub recompute the root/changed resources and acquire the server-derived all-or-none cell leases.
5. Propose/review/accept art with accepted-head compare-and-swap; client hash/resource omission is rejected.
6. Replay durable events or consume authenticated WebSocket notifications.
7. A configured desktop polls durable accepted-head events, imports hash-verified cells idempotently, persists its cursor, and fans the revision into matching local Studio sessions.
8. Record build attempts, release evidence, approvals, and transitions with audit/outbox/idempotency.

### Desktop and Studio flow

1. Start the desktop; its embedded loopback bridge becomes discoverable.
2. Select a validated project to bind project/place/channel hashes.
3. Open the Studio plugin and explicitly approve the exact pairing request in Settings.
4. Register, track, serialize, hash, and upload native cells through the verified bridge protocol.
5. Checkpoint directly from the plugin; the desktop imports verified bytes into CAS, records an idempotent capture receipt, overlays the accepted art state, and advances an eligible local channel head atomically.
6. Accepted local revisions fan out to every other paired Studio session without reopening. Small cells remain inline; larger cells use a short-lived session-scoped loopback download and are size/hash verified before the same dirty-safe atomic apply path.
7. Start, inspect, restart, or stop one checksum/version-pinned Rojo process for the selected manifest place. The renderer cannot supply an executable, checksum, PID, or port.
8. Sign in through the public-client OAuth flow and enumerate/probe authorized universes and exact places without exposing the token to the renderer.

## Explicit non-claims

- NeuMan does not merge RBXM/RBXL files as text and does not use Diversion or another proprietary VCS as the authority layer. Content-addressed immutable cells plus Hub leases are implemented so a future adapter can store large objects in Git LFS, S3, or another qualified backend without changing authority semantics.
- A Git merge is not an art acceptance, successful build, staging proof, or production deployment.
- OAuth login alone does not authorize API-key-only place publication.
- A reachable Rojo port is health evidence, not proof of Studio sync freshness or a releasable build.
- The SQLite/local-filesystem Hub profile is not the HA production profile.
- NeuMan has no official hosted Hub, account service, OAuth proxy, or central project database; remote team coordination is user-operated/self-hosted.
- The current code does not claim live Roblox Studio or production Roblox API qualification from unit tests.
- Native execution contracts and the crash-safe operator state machine are implemented; the fixed Luau runner, concrete Open Cloud adapter, and automatic Git+art assembly path are not yet implemented or production-qualified.
- Desktop resource management is read-only. Place publication remains operator-key-only, and current CLI preflight flags must not be treated as provider-derived authorization evidence.

## Release gate

Do not label 0.1.0 production-ready until every Phase-0 item in SPEC-22 has an evidence artifact: registered OAuth-client qualification, disposable-universe publishing/rollback/drift tests, Windows/macOS Studio golden corpus, pinned Rojo matrix, the selected Studio-plugin/Open Cloud assembly profiles, Hub production adapters and restore drill when that optional profile is shipped, installer signing/update tests, protocol fuzzing, and a completed security review.
