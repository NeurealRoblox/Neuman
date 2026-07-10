# Roblox OAuth resource provider

Status: implemented read-only desktop boundary; live provider qualification remains a release gate  
Primary specifications: SPEC-04, SPEC-05, SPEC-13, SPEC-15, SPEC-18

## Purpose

`roblox_resources.rs` is the only reusable HTTP boundary for Roblox universe/place discovery in the desktop application. It turns a short-lived OAuth access token into typed, non-secret resource metadata and fresh selection evidence. It has no mutation request type, no publication method, no database, and no serializable credential type.

`neuman_desktop.rs` is the only component allowed to read the OAuth record from Windows Credential Manager or macOS Keychain. It passes an in-memory redacted `OAuthSecret` to the provider. The Tauri renderer receives only `RobloxResourcePublicStatus`, inventory metadata, and exact selection evidence. The token, refresh token, client vault record, authorization headers, and token-resource form never cross the native command boundary.

## Fixed provider contract

The provider recognizes exactly two HTTPS origins, with redirects disabled:

- `https://apis.roblox.com` for OAuth token resources and stable Open Cloud reads;
- `https://develop.roblox.com` for the documented read-only legacy place index.

The exact operations are:

1. `POST https://apis.roblox.com/oauth/v1/token/resources` with the access token and public `client_id`, never a client secret.
2. `GET https://apis.roblox.com/cloud/v2/universes/{universe_id}` with OAuth Bearer authentication.
3. `GET https://develop.roblox.com/v1/universes/{universeId}/places?limit=100&sortOrder=Asc[&cursor=...]` without a credential, used only to discover candidate place IDs inside an already authorized universe.
4. `GET https://apis.roblox.com/cloud/v2/universes/{universe_id}/places/{place_id}` with OAuth Bearer authentication before a place becomes the selected provider-evidence target.

The required OAuth scope is `universe:read`. Identity still uses `openid profile`. The Roblox OpenAPI currently marks Get Universe and Get Place as stable and OAuth-capable. It exposes no stable Open Cloud List Places method. Consequently, the legacy index is never treated as authorization proof: its universe relationship is checked, and an exact selection must pass the OAuth Get Place read.

The OAuth resources response can contain `U`, meaning an owner-wide target grant without concrete universe IDs. NeuMan reports this limitation instead of inventing IDs. The user can enter a known numeric universe ID; NeuMan rechecks token resources, requires either that exact grant or `U`, then lets the exact OAuth Get Universe response establish access and identity.

## Bounds and validation

- Connect timeout: 10 seconds; total request timeout: 25 seconds.
- Response body: at most 1 MiB, enforced by both content length and streaming count.
- Authorization records: at most 100.
- Explicit universes: at most 50 per refresh.
- Place index: at most five pages and 500 unique places per universe.
- Numeric IDs: canonical positive decimal, at most 20 digits, no leading zero.
- Pagination cursor: printable ASCII, at most 512 bytes.
- Provider paths must exactly equal `universes/{id}` or `universes/{id}/places/{id}`.
- A place-index item must report the requested universe ID.
- Universe creator is at most one of `users/{id}` or `groups/{id}`.
- 3xx is a security failure; redirects are never followed.
- 401 requires session rotation or reauthorization; 403/404 are non-retryable access failures; 429/5xx are typed retryable provider availability failures.
- Provider error bodies are not reflected to the renderer. Request debug formatting redacts Bearer and form tokens.

## Native commands

- `roblox_resource_status`: returns only in-memory public state; it performs no network access.
- `refresh_roblox_resources`: refreshes the OS-vault session when its access token is within 60 seconds of expiry, then reads grants/universe metadata.
- `probe_roblox_universe`: validates a known universe ID, including owner-wide `U` grants.
- `select_roblox_place`: performs fresh exact Get Universe and Get Place reads and returns typed selection evidence.
- `clear_roblox_resource_selection`: clears only the in-memory public selection.

Every network command reloads the protected record natively. A locked, absent, corrupt, wrong-client, downgraded-scope, or uncommitted rotated record fails closed. Sign-out clears the inventory and selection.

## Publication boundary

`RobloxResourceCapabilities` and every `RobloxSelectionEvidence` state `operator-api-key-only` for place publishing. The desktop exposes no Place Publishing request, API-key input, arbitrary provider URL, arbitrary HTTP method, or generic CLI mutation. The Releases screen can create a local immutable intent plan, but actual publication remains in an operator-owned CI/secret-manager adapter using the API-key-only endpoint:

`POST https://apis.roblox.com/universes/v1/{universeId}/places/{placeId}/versions`

Fresh `RobloxSelectionEvidence` replaces renderer-entered claims for the universe path, place path, parent relationship, display metadata, provider update timestamps, observation time, and credential mode. It is evidence for planning and review, not a publication credential or proof that production was mutated.

## Tests

Deterministic mock-transport tests cover:

- exact endpoints, method order, sorting, and token redaction;
- stable inventory construction;
- exact OAuth-selected place evidence;
- owner-wide `U` behavior and exact probing;
- rejection before metadata reads for an ungranted universe;
- place-parent/path mismatch rejection;
- redirect rejection and strict origin validation.

Live P0 qualification must still test the reviewed public Roblox OAuth app, user-owned and group-owned universes, explicit and owner-wide grants, private/multi-place experiences, token rotation, provider throttling, revoked consent, and account switching on signed Windows and macOS builds.

## Official references

- [Roblox OAuth 2.0 reference](https://create.roblox.com/docs/cloud/auth/oauth2-reference)
- [Roblox Open Cloud OpenAPI document](https://create.roblox.com/docs/cloud/reference/openapi)
- [Roblox Open Cloud scopes](https://create.roblox.com/docs/cloud/reference/scopes)
- [Roblox universe APIs](https://create.roblox.com/docs/cloud/reference/features/universes)
- [Roblox place APIs](https://create.roblox.com/docs/cloud/reference/features/places)
- [Roblox Place Publishing guide](https://create.roblox.com/docs/cloud/guides/usage-place-publishing)
