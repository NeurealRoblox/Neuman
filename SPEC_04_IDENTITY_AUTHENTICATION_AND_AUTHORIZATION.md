# SPEC-04 — Identity, Authentication, and Authorization

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-03

## 1. Purpose

This specification defines human and automation identities, Roblox and GitHub authentication, local and Hub sessions, credential storage, account linking, project roles, authorization decisions, approvals, revocation, and audit obligations.

Authentication proves control of an identity. Authorization grants a specific action on a specific project resource. They MUST remain separate.

## 2. Identity model

### 2.1 Principal

A NeuMan Principal is an internal, opaque authorization subject. Types:

- `human`
- `service-account`
- `local-user`
- `studio-session`
- `system`

Fields:

- `principalId`
- `type`
- `displayName`
- `status: active | suspended | deleted`
- linked external accounts
- project memberships
- created/updated timestamps

Display name is not identity and may change.

### 2.2 ExternalAccount

- `provider: roblox | github | oidc`
- `providerSubject` — immutable provider subject (`sub` or numeric account ID)
- current username/display name/avatar URL as non-authoritative profile cache
- `linkedAt`, `lastVerifiedAt`
- `scopesGranted[]`
- `status: active | revoked | reauth-required`

Provider usernames MUST NOT be used as unique keys.

### 2.3 StudioPrincipal

A Studio session acts through its paired desktop principal. It is constrained by:

- pairing installation ID;
- Studio-reported user ID;
- active universe/place;
- selected project/workspace;
- session expiry;
- allowed plugin capabilities.

It cannot independently authorize a release or access cloud credentials.

## 3. Roblox OAuth

### 3.1 Client type

The desktop is a public OAuth client and MUST use Authorization Code with PKCE (`S256`). It MUST NOT embed, request, accept, or depend on a Roblox client secret. The official GitHub build compiles the registered public `client_id` from its protected release environment; the ID is not a credential. A source build MAY compile its own registered public ID. If no ID was compiled, development mode MAY accept a public ID for the current build/run, but an official build MUST NOT permit overriding its compiled client identity.

Required authorization request properties:

- exact registered `client_id`;
- exact registered `redirect_uri`;
- `response_type=code`;
- space-delimited minimum scopes;
- cryptographically random `state` with at least 128 bits of entropy;
- cryptographically random `nonce` with at least 128 bits of entropy when OIDC is requested;
- PKCE `code_verifier` containing 43–128 RFC 7636 unreserved characters;
- `code_challenge=BASE64URL(SHA256(verifier))` without padding;
- `code_challenge_method=S256`.

The system browser MUST be used. Embedded browser login is prohibited.

### 3.2 Redirect handling

The v1 desktop uses the single exact registered redirect `http://localhost:43891/oauth/callback`. It sends that `localhost` URI to Roblox but binds only `127.0.0.1:43891`. If the port is occupied, authorization fails before the system browser is opened. A random-port or custom-URI profile requires a later provider-qualified ADR and distinct registered redirect.

The callback handler MUST:

- bind loopback only;
- accept one response;
- enforce a 5-minute local transaction timeout, noting Roblox's authorization code itself has a shorter lifetime;
- verify state before exposing provider errors;
- reject duplicate, missing, or unexpected parameters;
- never log the code;
- close or invalidate the listener after completion.

### 3.3 Token exchange and validation

The client sends authorization code, code verifier, client ID, redirect URI as required by current Roblox documentation, and no client secret for public-client PKCE.

On response it MUST:

- require Bearer token type;
- calculate expiry using local receipt monotonic time with a 60-second safety margin;
- validate ID-token signature against the discovered JWKS;
- validate issuer, audience, expiry, issued-at tolerance, and nonce;
- use `sub` as the Roblox identity key;
- reject unexpected algorithms;
- store the newest rotating refresh token before invalidating the previous local record;
- zero transient secret buffers where supported.

OAuth discovery metadata SHOULD be fetched from the official discovery endpoint, cached for at most 24 hours, and pinned to an allowlisted HTTPS origin.

### 3.4 Token lifetimes and refresh

The implementation MUST use response lifetimes rather than hard-coded assumptions. Current documented defaults—15-minute access token and 90-day single-use rotating refresh token—are displayed only as diagnostics.

Refresh behavior:

- refresh at five minutes remaining or on first authorized request below that threshold;
- serialize refresh per account with a process lock;
- atomically persist the new refresh token;
- if a process crashes in the rotation window, attempt the safely retained candidate tokens according to a bounded recovery record;
- on `invalid_grant`, mark reauthorization required and stop mutation retries;
- never perform infinite refresh loops.

### 3.5 Scopes

Initial requested scopes SHOULD be:

- `openid`
- `profile`

`universe:read` is requested only when the user explicitly activates Roblox resource selection. Additional Creation & Productivity scopes are requested only for activated features and only if permitted for the registered app category. Scope upgrades require a new consent transaction. The effective scope set is shown in account settings.

Before a universe mutation, NeuMan MUST verify authorized resources using Roblox token resource information and/or the target endpoint response. Client-configured universe IDs do not grant access.

### 3.6 Roblox API keys

- The public desktop and Hub MUST NOT prompt for, receive, proxy, store, or validate a user's Roblox Open Cloud API key.
- Operator-owned CI MAY read a key from its deployment secret manager outside the public UI.
- Diagnostics MUST report only that an operator credential is configured, never its value or fingerprint unless the operator explicitly enables a safe last-four-style identifier.

### 3.7 Logout and revocation

Logout:

1. revoke the refresh token when possible;
2. delete access/refresh/ID tokens and recovery records from keychain;
3. clear in-memory tokens;
4. invalidate local authorized operations;
5. retain minimal non-secret account identity only if user chooses account history.

Provider revocation discovered during an operation transitions the account to `reauth-required` and fails the operation with a typed error.

## 4. GitHub authentication

### 4.1 GitHub App

NeuMan SHOULD use a GitHub App with fine-grained repository installation permissions. A PAT is not a supported primary flow.

Desktop user authorization may use GitHub App device flow or browser authorization. Device flow:

- displays the exact GitHub verification origin;
- never asks the user to paste a GitHub password or token;
- honors provider polling interval and `slow_down` responses;
- expires cleanly;
- stores tokens in the OS keychain.

### 4.2 Git transport

Local Git fetch/push SHOULD use the user's existing SSH agent or Git Credential Manager. NeuMan MUST NOT scrape credentials from Git configuration or process output. If App tokens are used for HTTPS transport, they are injected through an ephemeral credential helper or askpass channel, never a command-line argument or remote URL.

### 4.3 GitHub App permissions

Baseline:

- Metadata: read
- Contents: read; write only for enabled commit/PR features
- Pull requests: read/write
- Checks: read/write
- Actions: read only if displayed

Administration, members, secrets, workflows write, and organization-wide permissions are denied by default. A permission increase requires explicit installation review.

## 5. Hub authentication

### 5.1 Human login

Self-hosted Hub supports pluggable OIDC. Reference profiles:

- GitHub identity via GitHub App;
- generic enterprise OIDC;
- local development identity only when Hub runs in explicit development mode.

Production Hub MUST NOT provide password authentication in v1.

### 5.2 Desktop session

Hub issues:

- short-lived access token, target 15 minutes;
- rotating refresh token, target 30 days maximum;
- device/session ID;
- authorized project membership snapshot version.

Tokens are audience-bound to the Hub deployment and sender-constrained if a future standard is adopted. Local storage follows OS keychain rules.

### 5.3 Service accounts

Service accounts use scoped, rotating credentials stored in a deployment secret manager. They cannot use human approval permissions. Each credential has an expiry, owner, purpose, last-used timestamp, and revocation path.

## 6. Account linking

Linking Roblox and GitHub to one NeuMan principal requires an authenticated transaction for both accounts within 10 minutes. It records consent and external immutable subject IDs.

Rules:

- one external account maps to at most one active principal per Hub;
- unlinking is blocked if it would remove the only production approval identity required by policy without admin reassignment;
- merges between principals require administrator review and produce an audit event;
- Studio user ID mismatch with paired Roblox identity is a warning or policy error, never silently relinked.

## 7. Project roles

Reference roles:

- `viewer`
- `artist`
- `developer`
- `reviewer`
- `release-manager`
- `production-approver`
- `project-admin`
- `hub-auditor`

Permissions:

| Action | viewer | artist | developer | reviewer | release mgr | prod approver | project admin |
|---|---:|---:|---:|---:|---:|---:|---:|
| Read project/revisions/builds | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Acquire art lock |  | ✓ | optional |  |  |  | ✓ |
| Submit art proposal |  | ✓ | optional |  |  |  | ✓ |
| Accept art proposal |  | policy |  | ✓ |  |  | ✓ |
| Create build |  |  | ✓ | ✓ | ✓ |  | ✓ |
| Create staging release |  |  | optional |  | ✓ |  | ✓ |
| Approve production |  |  |  |  | policy | ✓ | policy |
| Publish production |  |  |  |  | ✓ | policy | ✓ |
| Change ownership/policy |  |  |  |  |  |  | ✓ |

The table is baseline. Project policy MAY make permissions stricter, never broader than a principal's deployment-level allowance.

## 8. Authorization decision

Every mutation evaluates:

```text
principal active
AND session active
AND project membership current
AND action granted by role
AND resource/environment condition satisfied
AND external provider permission sufficient
AND approval/lock/base-revision gates satisfied
AND compatibility/security policy satisfied
```

Result:

- `allow`
- `deny(code, reason)`
- `requires-step-up(requirements)`
- `requires-approval(policy)`

Authorization results MUST NOT be cached beyond membership/policy version changes or token expiry.

## 9. Step-up and approvals

Production-impact actions SHOULD require recent authentication within 15 minutes. An approval contains:

- approver principal;
- authenticated account evidence;
- release/art proposal identity and immutable hash;
- policy revision;
- decision and optional comment;
- timestamp;
- signature/MAC by Hub audit service.

Approval becomes invalid if immutable request content changes. Two-person rules require distinct principals; multiple external accounts for one principal do not count twice.

## 10. Local mode authorization

Local mode has one `local-user` principal. It may perform local capture/build/test actions. Production publication still relies on Roblox's signed-in account and explicit confirmation. Local mode cannot claim server-enforced team approvals or locks.

## 11. Credential storage

Windows: Credential Manager/DPAPI-backed storage.  
macOS: Keychain with application access controls.  
Linux desktop OAuth: unsupported in v1; CLI/Hub operation MUST NOT imply a qualified desktop token store.  
Hub: deployment secret manager/KMS; database stores references or encrypted envelopes.

Credential records include provider, account subject, created/rotated/expiry timestamps, and opaque secret bytes. They exclude display data not necessary for retrieval.

File permissions are not an acceptable substitute for keychain storage. OAuth tokens MUST NOT fall back to a project file, preference file, SQLite, browser storage, environment variable, command argument, renderer state, or Studio plugin. A locked, denied, unavailable, or corrupt backend produces a typed sign-in/restore failure and no durable session.

## 12. Session security

- Access sessions have absolute and idle expiry.
- Refresh reuse after rotation triggers revocation of the session family where supported.
- Clock skew tolerance is at most five minutes for signed tokens; operations use server time when connected.
- Session listings show device, approximate last use, creation, and revoke action.
- Suspending a principal revokes Hub sessions and blocks new locks/approvals immediately.

## 13. Audit requirements

Audit events are required for:

- login, logout, refresh failure, revocation;
- account link/unlink/merge;
- role/membership change;
- permission or scope change;
- production step-up and approval;
- service-account creation/rotation/revocation;
- denied high-impact actions;
- use of operator CI publication mode.

Audit data MUST exclude raw tokens and authorization codes.

## 14. Privacy

- Store immutable provider subject plus minimum profile cache.
- Avatar URLs are optional and refreshable.
- Do not create cross-platform behavioral profiles.
- Do not use Roblox-derived user data for model training.
- Support account data export and deletion/pseudonymization consistent with audit retention.

## 15. Error codes

- `IAM_LOGIN_CANCELLED`
- `IAM_STATE_MISMATCH`
- `IAM_NONCE_MISMATCH`
- `IAM_PKCE_FAILED`
- `IAM_TOKEN_INVALID`
- `IAM_TOKEN_EXPIRED`
- `IAM_REAUTH_REQUIRED`
- `IAM_SCOPE_MISSING`
- `IAM_RESOURCE_NOT_GRANTED`
- `IAM_ACCOUNT_LINK_CONFLICT`
- `IAM_SESSION_REVOKED`
- `IAM_PERMISSION_DENIED`
- `IAM_STEP_UP_REQUIRED`
- `IAM_APPROVAL_REQUIRED`
- `IAM_API_KEY_NOT_ACCEPTED`

## 16. Acceptance criteria

1. OAuth conformance tests cover state, nonce, PKCE, redirect, JWKS rotation, clock skew, and refresh rotation crash recovery.
2. No token appears in command lines, logs, crash dumps, support bundles, or plugin settings.
3. A public UI contains no Roblox API-key input.
4. Role and policy decisions have table-driven tests for every action/environment.
5. Approvals invalidate on content change and cannot double-count one principal.
6. Account linking handles collisions and mismatch without implicit merge.
7. Logout and administrative suspension revoke all applicable sessions.
8. Binary/string inspection proves there is no desktop client-secret path and the official client ID came from the reviewed build configuration.
9. Windows Credential Manager and macOS Keychain pass store/restore/delete/locked-backend qualification without plaintext fallback; unsupported desktop platforms reject OAuth persistence.

## 17. References

External sources last verified: 2026-07-09.

- [Roblox OAuth overview](https://create.roblox.com/docs/cloud/auth/oauth2-overview)
- [Roblox OAuth reference](https://create.roblox.com/docs/cloud/auth/oauth2-reference)
- [Roblox third-party app policy](https://en.help.roblox.com/hc/en-us/articles/37924211313044-Creator-Third-Party-App-Policy)
- [GitHub App user authorization](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app)
