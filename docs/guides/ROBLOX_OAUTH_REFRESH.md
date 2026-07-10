# Roblox public-client refresh rotation

Source: `roblox_oauth.rs`  
Specification: `/docs/specs/SPEC_04_IDENTITY_AUTHENTICATION_AND_AUTHORIZATION.md`

## Boundary

This module performs one Roblox OAuth public-client refresh and validates the resulting identity. It has no `client_secret` field or request path, performs no credential persistence, contains no keyring fallback, and does not update desktop/UI state. `neuman_desktop.rs` is the privileged caller that performs the OS-vault transaction and returns only redacted account state to the renderer.

The caller supplies a compiled/otherwise trusted public `client_id`, required scopes, an in-memory protected refresh token, and the immutable Roblox `sub` stored during the original authorization. `RobloxRefreshContext` is consumed by a refresh call to discourage accidental reuse of a one-time rotating credential.

On success the caller receives `RotatedRobloxTokens`. It must atomically persist the new refresh token and complete its crash-recovery transaction outside this module before discarding the safe recovery record. This module never retries a refresh, falls back to the old token, or writes either token anywhere.

## Refresh flow

`RobloxOAuthRefresher::refresh` performs exactly these steps:

1. Fetch the fixed `https://apis.roblox.com/oauth/.well-known/openid-configuration` URL.
2. Require issuer `https://apis.roblox.com/oauth/` and pin token, userinfo, and JWKS endpoints to the exact `https://apis.roblox.com` origin.
3. POST form fields `grant_type=refresh_token`, public `client_id`, and the protected refresh token. A client secret cannot be represented.
4. On `invalid_grant`, return `ReauthenticationRequired`, `retryable=false`, and stop. Provider descriptions are ignored rather than copied into errors.
5. Require a non-empty access token, case-insensitive Bearer type, configured minimum `expires_in`, and all required effective scopes.
6. Require a new non-empty refresh token different from the consumed token. Missing or reused refresh credentials are security failures.
7. Fetch userinfo with the new bearer token and require its immutable `sub` to equal the stored subject.
8. If an ID token was returned, fetch JWKS and require ES256, matching `kid`, EC/P-256 signing key, exact issuer, public-client audience, unexpired claims, sane issued-at time, and the same subject. Refresh verification intentionally does not require or compare the original authorization nonce.
9. Return the rotated secrets and validated user/scope/lifetime metadata without serialization or persistence.

An omitted refresh ID token is accepted because it is optional in the refresh response; userinfo subject validation remains mandatory. If an ID token is present, it cannot be ignored.

## Transport profile

`ReqwestRobloxOAuthTransport` is the production transport:

- HTTPS-only client;
- redirects disabled (`Policy::none`);
- 10-second connect timeout and 30-second total request timeout;
- one-MiB hard body limit checked from `Content-Length` and while streaming;
- JSON `Accept` header;
- no arbitrary error strings returned across the transport boundary.

Every URL is revalidated against the pinned Roblox HTTPS origin before transport execution. A redirect is returned as a non-success response and is never followed.

`RobloxOAuthTransport` separates network behavior from validation. Deterministic tests inject bounded responses while exercising the same request construction and pure validators.

## Secret handling

`OAuthSecret`, refresh context, token results, HTTP requests, HTTP responses, and errors use redacted `Debug` implementations. Raw response bodies report only their length in debug output. Errors use stable static messages and never include token endpoint bodies, `error_description`, bearer values, refresh tokens, access tokens, or ID tokens.

Secret-bearing types have no serde implementations. `expose_secret` is explicit and intended only for the concrete transport or the caller's protected atomic credential-vault transaction. `RotatedRobloxTokens` is not suitable for returning to a webview.

## Caller integration

```rust
let config = RobloxPublicClientConfig::recommended(compiled_client_id)?;
let context = RobloxRefreshContext::new(restored_refresh_token, stored_sub)?;
let refresher = RobloxOAuthRefresher::new(config, ReqwestRobloxOAuthTransport::new()?);

let rotated = refresher.refresh(context, now_unix_seconds).await?;
// Atomically persist rotated.refresh_token() and the associated token record
// in the OS credential vault. Do not expose the result to the renderer/plugin.
```

The desktop integration serializes refresh through its native command/state boundary, owns the keyring replacement transaction, and updates only public signed-in/reauth-required state. The Settings “Rotate session” action exercises this path; future Roblox resource commands must invoke the same path on demand before expiry. No Studio plugin or webview receives these token objects. A durable recovery journal is intentionally not stored outside the credential vault: if the rotated credential cannot be committed, the operation fails closed and requires interactive reauthorization rather than risking plaintext recovery material or reuse of a single-use token.

## Errors

Important stable codes include:

- `ROBLOX_OAUTH_REAUTH_REQUIRED` — `invalid_grant`; interactive sign-in required, no retry.
- `ROBLOX_OAUTH_REFRESH_ROTATION_MISSING` — provider omitted or malformed the replacement token.
- `ROBLOX_OAUTH_REFRESH_NOT_ROTATED` — provider returned the consumed refresh token.
- `ROBLOX_OAUTH_TOKEN_TYPE_INVALID`
- `ROBLOX_OAUTH_TOKEN_LIFETIME_INVALID`
- `ROBLOX_OAUTH_SCOPE_DOWNGRADE`
- `ROBLOX_OAUTH_SUBJECT_MISMATCH`
- `ROBLOX_OAUTH_ID_TOKEN_INVALID`
- `ROBLOX_OAUTH_ORIGIN_REJECTED`
- `ROBLOX_OAUTH_BODY_TOO_LARGE`
- `ROBLOX_OAUTH_PROVIDER_UNAVAILABLE`

Only transport/temporary-provider failures are retryable. Rotation, subject, signature, scope, origin, and provider-contract failures are not retried inside the module.

## Verification

Six focused test groups pass against the full NeuMan library:

- discovery issuer and endpoint-origin pinning, including hostile-origin rejection;
- mandatory refresh rotation, Bearer type, lifetime, and scope validation;
- valid ES256/JWKS verification without a nonce, wrong subject rejection, and signature tamper rejection;
- end-to-end request construction proving no `client_secret`, same-subject userinfo, and debug redaction;
- subject-change rejection and `invalid_grant` reauthentication classification without provider-description leakage;
- optional returned ID-token verification after userinfo correlation.

The full default and desktop-feature Clippy gates pass with warnings denied. `rustfmt --check` passes for the complete crate.

## Qualification status

The implementation is provider-shaped and deterministic, but Roblox refresh rotation, exact response behavior, JWKS rotation/cache behavior, throttling, resource scopes, and revocation still require the SPEC-20 **P0-01 live Roblox OAuth qualification** against an approved disposable Roblox OAuth application. No production-support claim should be made until that evidence is recorded.
