# NeuMan desktop

Status: focused Roblox account-connection milestone  
Specifications: SPEC-04, SPEC-05, SPEC-06, SPEC-08, SPEC-13

The desktop is currently intentionally single-purpose: it connects one Roblox account through first-party OAuth, protects the resulting session, restores it on launch, and supports cancellation and revocation. Project, resource, Studio, build, and release surfaces remain hidden until account connection is qualified. `neuman_desktop.rs` is the privileged boundary; the renderer never receives provider tokens.

## Run

```text
npm ci
npm run build
npm run dev:desktop
```

Build a standalone unsigned development executable, with the renderer embedded,
using the Tauri build pipeline:

```text
npm run build:desktop:debug
```

Do not hand out an executable produced by a direct debug `cargo build`. Tauri
debug binaries use `build.devUrl` and therefore require the Vite development
server. The Cargo package is named for the desktop target while the library
retains the `neuman` crate name; the explicit `desktop` feature and runner's
`--bin neuman-desktop` argument prevent Tauri from selecting the sibling
`neuman` CLI in this multi-binary package.

The Rust-only native check is:

```text
cargo check --features desktop --bin neuman-desktop
```

Set `NEUMAN_CLI` only when the sibling `neuman` binary is not beside the desktop executable. Set `NEUMAN_HUB_URL` before startup only to declare a user-operated/self-hosted Hub endpoint; there is no official NeuMan Hub service or central database. The current renderer treats it as deployment configuration and does not accept arbitrary runtime URLs.

## Roblox first-party OAuth

Register a Roblox OAuth 2.0 public client with this exact redirect URL:

```text
http://localhost:43891/oauth/callback
```

Compile a source build with `NEUMAN_ROBLOX_OAUTH_CLIENT_ID` set to that public client ID. Official GitHub builds receive the reviewed public ID from the protected release environment and do not expose an override. A public client ID is not a secret; no client secret is accepted or embedded.

The executable binds `127.0.0.1:43891`, sends the registered `localhost` URI, and requires the callback Host header and path to match. If another process owns the port, authorization fails before the browser is opened.

The flow uses:

- authorization code grant;
- S256 PKCE with a 64-byte random verifier;
- independent 256-bit state and nonce values;
- identity-only `openid profile` scopes;
- a five-minute callback timeout and 8 KiB request bound;
- one callback request over numeric loopback;
- issuer, endpoint-origin, ES256, P-256 key, signature, audience, expiry, nonce, subject, and user-info subject validation;
- HTTPS-only provider requests with bounded timeouts;
- OS credential-vault storage through `keyring`: Windows Credential Manager or macOS Keychain on the supported desktop platforms;
- best-effort refresh-token revocation followed by local credential deletion on sign-out.

No client secret is embedded or requested. Missing, locked, denied, or corrupt vault storage fails sign-in/restore without a plaintext, project-file, SQLite, renderer, or environment-variable token fallback. The later Roblox-resource milestone requires a fresh consent transaction adding `universe:read`; account connection does not preemptively request it.

## Roblox resources

The Roblox resources view invokes a native, read-only provider boundary. The backend loads the short-lived access token from the OS vault (rotating it when near expiry), calls Roblox token resources, and returns only typed universe/place metadata. The renderer never receives a token or authorization header.

Concrete token-resource targets are enumerated automatically. Roblox owner-wide `U` targets do not contain universe IDs, so the view also supports an exact numeric universe probe. Candidate places come from Roblox's documented read-only place index; clicking a place performs a fresh stable OAuth Get Place call before recording the selection. The resulting typed evidence contains canonical provider paths, parent relationship, metadata/update times, observation time, and the fixed `operator-api-key-only` publishing declaration.

There is deliberately no edit or publish control in this view. See `ROBLOX_RESOURCE_PROVIDER.md` for the fixed endpoints, bounds, evidence model, limitations, tests, and live qualification matrix.

Roblox OAuth remains provider-beta functionality. A release must qualify the registered client, granted scopes, callback behavior, token refresh/rotation, revocation, account switching, consent review, and resource enumeration against the current official provider before public distribution.

## Embedded Studio bridge

Desktop startup launches the same loopback-only `BridgeService` used by `neuman-bridge`. It owns discovery port `127.0.0.1:34873` and an OS-selected loopback WebSocket port. The renderer receives only redacted status and pairing metadata.

Pairing sequence:

1. The plugin discovers the bridge and displays its six-digit code.
2. The native backend records the challenge and exact plugin installation ID.
3. Settings displays the pending request, Studio version/platform metadata, and matching code.
4. The user explicitly approves that exact challenge/plugin tuple.
5. The plugin retries and receives its random scoped bridge credential.

Selecting a validated workspace computes a context binding from project ID, manifest hash, ownership hash, release-policy hash, default place, authoring target, and default art channel. Without a default place/channel the desktop deliberately leaves mutation traffic unbound.

The embedded desktop adapter commits a verified capture to CAS and an immutable transfer/capture receipt before acknowledging it. It enforces ownership and context, overlays the accepted head, advances only eligible local zero-approval channels, and fans accepted cells only to sessions whose project/place/channel and workspace generation still match, while excluding the source. Protected channels remain proposals. When a credential-free `providers.hub` declaration, exact startup `NEUMAN_HUB_URL`, and OS-vault bearer are present, the native-only adapter in `HUB_DESKTOP_ADAPTER.md` uploads canonical full-state manifests, proposes changed cells, and imports accepted cross-machine revisions before invoking the same Studio apply path.

## Allowlisted desktop operations

- connect a directory containing `neuman.project.yaml`;
- validate the manifest;
- inspect project status;
- fetch the configured Git remote and fast-forward the clean attached branch only;
- inspect the embedded Studio bridge;
- start, list, inspect, restart, and stop the checksum/version-pinned Rojo session resolved from the selected workspace manifest and lock;
- import one project-relative native RBXM cell into an immutable proposed revision;
- build from an exact art revision and optional project-relative assembled candidate;
- create an immutable release plan from an exact bundle/environment/place;
- start/status/refresh/revoke Roblox OAuth;
- enumerate/probe authorized Roblox universes, inspect candidate places, and OAuth-validate an exact read-only place selection;
- approve an exact pending Studio pairing request.

Artifact paths are project-relative, canonicalized, required to remain inside the selected workspace, and required to be regular files. Manifest keys, provider IDs, Git refs, messages, and hashes receive length/character checks before the CLI is invoked. The operator Roblox publication key has no renderer field and no command-line flag.

## Renderer behavior

The renderer has Portfolio, Workspace, Roblox resources, Art revisions, Builds, Releases, Activity, and Settings views. Build and release controls remain disabled until their explicit immutable identifiers are present. CLI result envelopes are parsed into user-safe summaries; the current-session timeline is not presented as the durable audit ledger.

Browser-only Vite preview uses a mock backend and performs no mutation. It is for visual testing, not a security or provider test.

## Packaging

`build.rs` emits a valid deterministic bootstrap PNG/ICO into Cargo `OUT_DIR`. This avoids checking an opaque placeholder binary into the source bootstrap. The Tauri updater plugin and canonical GitHub release endpoint are configured, but the real updater public key is deliberately absent and release preflight blocks until it is provisioned. Production packaging must provide signed brand assets, code signing/notarization, SBOM/provenance, updater signature and rollback testing, platform installer tests, crash redaction, and OS-vault qualification. `OFFICIAL_RELEASES.md` is the normative distribution contract; the protected GitHub workflow sources are `GITHUB_CI_WORKFLOW.yml` and `GITHUB_OFFICIAL_RELEASE_WORKFLOW.yml`.
