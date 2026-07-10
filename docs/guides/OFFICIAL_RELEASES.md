# Official build, signing, and provenance contract

Status: Normative for official distribution  
Version: 1  
Last updated: 2026-07-09

## 1. Product and service boundary

NeuMan is an open-source desktop application. The NeuMan project does not operate an account service, OAuth callback proxy, project database, multi-tenant Hub, art store, telemetry collector, or release control plane for users.

The desktop communicates directly with the user's selected Roblox, Git, GitHub, Studio, and optional self-hosted Hub endpoints. Local SQLite databases, content-addressed objects, project files, and OS-vault credentials remain on the user's machine. A team may deploy the open-source Hub into infrastructure it owns. A third party may offer compatible hosting under its own identity, but that service is not an official NeuMan runtime dependency and does not make its builds official.

GitHub is used for source review, CI, release assets, checksums, and cryptographic provenance. Reading an update manifest or downloading a release asset from GitHub is distribution, not a central project-data service. The app continues to open local projects when GitHub is unavailable.

## 2. What “official” means

An artifact is an **official NeuMan build** only when all of the following are true:

1. It is attached to a release in the canonical public GitHub repository.
2. The release refers to a protected, annotated, GitHub-verified signed tag.
3. The tag resolves to the exact commit recorded in `release-evidence.json`.
4. The artifact hash appears in `SHA256SUMS`.
5. `gh attestation verify` validates its GitHub artifact attestation against the canonical repository, the reviewed `official-release.yml` signer workflow, and the workflow-source commit recorded in `release-evidence.json`.
6. Windows installers have a valid, timestamped Authenticode chain to the published NeuMan signer.
7. macOS bundles have a valid Developer ID signature, successful Apple notarization, and a stapled ticket.
8. Updater bundles have a valid Tauri updater signature under a public key embedded in the prior trusted app.
9. The protected release workflow and required reviewers completed without a waived signing, provenance, or version gate, and the published release is locked by GitHub immutable releases.

A locally compiled binary, fork build, pull-request artifact, nightly test package, or manually uploaded file is not official even if its source is unchanged. Forks may sign their own builds but must use their own identity and update keys.

## 3. OAuth build contract

The desktop is a Roblox OAuth **public client**. Official builds compile the registered public `client_id` from the protected GitHub environment variable `ROBLOX_OAUTH_CLIENT_ID`. This identifier is public by design. No `client_secret` exists in source, workflow configuration, binary resources, environment requirements, token exchange, or support tooling.

The only supported interactive flow is authorization code with S256 PKCE through the system browser. The callback is the exact registered URI `http://localhost:43891/oauth/callback`; the listener binds numeric loopback, accepts one bounded request, validates Host/path/state, and expires. Source builds may compile their own public client ID with `NEUMAN_ROBLOX_OAUTH_CLIENT_ID`. If none was compiled, development UI may accept a public client ID for that run; an official build never permits overriding its compiled identity.

OAuth access, refresh, and ID tokens are serialized only into the OS credential service:

- Windows: Credential Manager;
- macOS: Keychain Services;
- Linux: no supported desktop OAuth profile in v1; Linux remains CLI/Hub-only until Roblox Studio and a credential backend are qualified.

If the backend is missing, locked, denied, or corrupt, durable sign-in fails closed. There is no plaintext file, SQLite, browser local-storage, environment-variable, Studio-plugin, or project-manifest fallback. Project logs, diagnostics, renderer commands, and support bundles receive only redacted status.

## 4. Repository controls

The canonical repository MUST configure:

- default-branch protection with pull requests and required CI;
- required review by code owners for authentication, update, signing, workflow, and release files;
- a tag ruleset for `v*` that prevents deletion, update, and untrusted creation;
- signed annotated release tags with GitHub verification;
- an `official-release-signing` environment containing signing secrets and at least one required reviewer;
- a separate `official-release-publish` environment with a required release reviewer;
- both environments restricted to the protected default branch, with no custom deployment branch or tag permitted;
- immutable releases enabled so a published tag and its assets cannot be changed;
- Actions restricted to approved actions, with every action reference pinned to a full commit SHA;
- no self-hosted runner for signing unless its image, access, persistence, and incident response have an approved threat model;
- secret scanning, dependency review, branch protection, and audit-log retention appropriate to the organization;
- `GITHUB_TOKEN` permissions declared per job and denied by default elsewhere, including `artifact-metadata: write` only in jobs that create v4 attestations.

The signing workflow is manual-dispatch only. It first requires the canonical repository, the protected default branch, and the current default-branch HEAD as the workflow source. It then validates a canonical tag and a separately typed full commit SHA through the GitHub API before checking out the target. The signed tag object's embedded name must equal the requested ref, its verification reason must be `valid`, and it must point directly to both the expected commit and the current default-branch HEAD. This makes the attested workflow-source digest and release source digest identical instead of trusting release scripts from a different revision. It never runs on `pull_request`, `pull_request_target`, a fork, an arbitrary branch, a stale workflow revision, an older detached release commit, or an unverified lightweight tag.

## 5. Required release configuration

Repository/environment variable:

- `ROBLOX_OAUTH_CLIENT_ID`: reviewed public-client ID registered with the exact loopback redirect.
- `APPLE_TEAM_ID`: reviewed 10-character Team ID published with the macOS signing identity.

Secrets in `official-release-signing`:

- `WINDOWS_CERTIFICATE`: base64 PKCS#12 containing the Authenticode certificate/private key;
- `WINDOWS_CERTIFICATE_PASSWORD`: PKCS#12 export password;
- `APPLE_CERTIFICATE`: base64 PKCS#12 containing a Developer ID Application certificate/private key;
- `APPLE_CERTIFICATE_PASSWORD`: PKCS#12 export password;
- `APPLE_API_ISSUER`: App Store Connect issuer ID;
- `APPLE_API_KEY`: App Store Connect key ID;
- `APPLE_API_PRIVATE_KEY`: base64 contents of the corresponding `.p8` key;
- `TAURI_SIGNING_PRIVATE_KEY`: Tauri updater private key;
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: updater key password.

The matching Tauri updater public key is public configuration, not a secret. It MUST be embedded in `plugins.updater.pubkey`, the updater plugin MUST be registered in the signed desktop, and the only configured endpoint for the official channel is `https://github.com/NeurealRoblox/Neuman/releases/latest/download/latest.json`. The release preflight deliberately fails until all three conditions are present. `dangerousInsecureTransportProtocol` is forbidden.

The public certificate identities, updater public key, and retirement dates MUST be documented before the first beta. Signing jobs import credentials only into ephemeral runner stores, mask derived secrets, remove temporary files, and destroy temporary keychains/certificate-store entries in `always()` cleanup steps.

Where available, the project SHOULD migrate Windows signing to an HSM-backed or federated/OIDC signing service so the private key is non-exportable. That change requires an ADR and must preserve Authenticode/timestamp verification; it does not weaken any release gate.

## 6. Workflow stages

1. **Workflow and tag preflight:** require the canonical repository and current protected default-branch workflow source; validate tag grammar; require an annotated tag object whose signed name matches the ref; require GitHub verification reason `valid`; resolve one commit; and compare the operator-entered SHA.
2. **Source preflight:** verify Cargo/npm/Tauri/lockfile versions equal the tag; check pinned Rust/Node/action commits; enforce PKCE/public-client/OS-vault/updater markers; reject a client-secret path, hosted telemetry dependency, deployment action, unsafe release trigger, or self-hosted signer. CI also validates documentation layout and runs `scripts/release/contract-test.mjs`, a network-free generation/verification/tamper test over disposable release assets.
3. **Quality:** run formatting, Clippy with warnings denied, all locked Rust targets, TypeScript checking, and the production renderer build from a clean checkout.
4. **Windows signing build:** import the certificate, configure the exact thumbprint and SHA-256 timestamping, compile the public OAuth ID, build updater artifacts, and require valid timestamped Authenticode on every MSI/EXE selected for release.
5. **macOS arm64 signing build:** use an ephemeral keychain, select the reviewed Team ID from an ephemeral keychain, use App Store Connect API notarization, compile the same OAuth ID, build updater artifacts, and require `codesign`, exact Team ID, Developer ID authority, Gatekeeper assessment, and stapler validation. Intel macOS is not an official v1 target.
6. **Updater signing:** Tauri signs every selected update bundle with the independent updater key. Every `.sig` must be non-empty and have an exact sibling asset. The draft job creates deterministic `latest.json` entries only for `windows-x86_64` and `darwin-aarch64`, using immutable tag-specific GitHub download URLs and the exact signature contents.
7. **Checksums and provenance:** create platform and aggregate SHA-256 manifests; generate schema-v2 `release-evidence.json`; and attest files in the job that actually produced them. Platform jobs attest platform artifacts, while the draft job separately attests the Studio plugin, updater manifest, evidence, and aggregate checksum. Downloaded binaries are not misleadingly re-attested as draft-job build products.
8. **Draft:** download only artifacts whose name includes the current workflow run ID and attempt, merge them with collision detection, and create a new draft GitHub Release. A draft is not called immutable; existing releases are never clobbered and assets are never uploaded manually.
9. **Independent publish gate:** require the expected release to remain a draft; redownload every asset; verify aggregate checksums; reconstruct and compare the updater/evidence inventory; verify attestations against the canonical repository, exact signer workflow and workflow-source commit while denying self-hosted runners; require the publish environment approval; then remove draft status. The job polls `gh release verify` for GitHub's immutable-release evidence. If verification never appears, it returns the mutable release to draft when possible and fails; only a successfully verified immutable release satisfies the official-build contract.

Any missing secret, public updater key, signature pair, timestamp, notarization ticket, updater manifest, checksum, attestation, tag verification, workflow identity, or version match is a hard failure. A retry uses artifact names containing the new `run_attempt`; the draft job cannot mix or silently reuse outputs from an earlier attempt.

## 7. Published evidence

Every release includes:

- Windows installer/updater assets and `.sig` files;
- macOS DMG/updater archive and `.sig` files;
- the versioned Studio plugin source artifact;
- `SHA256SUMS.windows` and `SHA256SUMS.macos`;
- aggregate `SHA256SUMS`;
- `latest.json`, containing only signed update bundles served directly from immutable tag-specific GitHub Release URLs;
- `release-evidence.json` with tag, source commit, workflow-source commit, signer workflow, repository, workflow-attempt URL, supported updater targets, byte size, SHA-256, and expected signature class for every non-self-referential candidate asset;
- GitHub artifact attestations discoverable from the canonical repository;
- generated release notes and links to source/specification state.

SBOM and vulnerability evidence remain mandatory for public beta under SPEC-20/22. Until the repository adds and qualifies the SBOM generator, releases are development artifacts and MUST NOT be labeled public beta or stable.

## 8. Consumer verification

After downloading an asset and `SHA256SUMS` from the same release:

```text
sha256sum -c SHA256SUMS
gh attestation verify PATH_TO_ASSET -R NeurealRoblox/Neuman \
  --signer-workflow NeurealRoblox/Neuman/.github/workflows/official-release.yml \
  --signer-digest WORKFLOW_SOURCE_COMMIT --deny-self-hosted-runners
```

Windows users additionally inspect `Get-AuthenticodeSignature` or file Properties and match the published subject/thumbprint. macOS users run `codesign --verify --deep --strict`, `spctl --assess --type execute`, and `xcrun stapler validate` on the application bundle. The app's updater validates its separate embedded updater public key before installation; HTTPS or a matching filename alone is never trust evidence.

## 9. Rotation, compromise, and revocation

- Native code-signing renewal with the same identity is announced and verified in a rehearsal release.
- Updater-key rotation requires an overlap release trusted by the old key that embeds the new public key; losing the old key requires manual reinstall and a security advisory.
- A suspected signing-key or GitHub-environment compromise freezes publication, revokes/rotates affected credentials, removes update metadata, preserves forensic workflow evidence, and publishes a signed advisory through unaffected channels.
- A compromised artifact is never replaced under the same tag/name. The release is withdrawn and a new patch version is built from a reviewed commit.
- Expired timestamps, revoked certificates, failed notarization, unverifiable attestations, and unknown provenance are treated as failures, not warnings.

## 10. Activation checklist

The checked-in workflow is fail-closed and intentionally cannot produce an official release until maintainers configure the canonical repository, protected environments, immutable releases, public OAuth application, Apple Team ID, platform certificates, notarization key, updater key/public-key, and required reviewers. The updater plugin and canonical GitHub endpoint are registered, but the repository intentionally contains no placeholder verification key; `scripts/release/preflight.mjs` reports the missing real public key before any signing job can run. The first release is a rehearsal in disposable accounts; the public-beta label remains blocked by the wider SPEC-22 qualification matrix.

Workflows are active only from `.github/workflows`. The CI and official-release definitions are checked in directly at those GitHub-recognized paths; release preflight validates the installed workflow rather than a duplicate template.

The reviewed action commits were resolved from the official action repositories on 2026-07-10: checkout v7.0.0, setup-node v6.4.0, attest v4.1.1, upload-artifact v7.0.1, and download-artifact v8.0.1. Updating any pin is a security-sensitive source change and requires reviewing the new upstream commit before changing both the workflow and preflight allowlist.
