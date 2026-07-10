# SPEC-12 — GitHub App Integration

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-04, SPEC-11

## 1. Purpose

This specification defines the GitHub App, repository installation binding, user and installation authentication, permissions, APIs, webhooks, checks, pull-request workflows, branch protection interaction, rate limiting, GitHub Enterprise behavior, and failure handling.

## 2. App model

Use a GitHub App rather than a broad OAuth App or PAT. The official NeuMan project does not operate a shared GitHub App backend. Deployments are:

- operator-created App for each self-hosted Hub;
- development App isolated from production;
- third-party managed App only when the user explicitly chooses that external provider under its own identity and terms.

Each has distinct client/app IDs, keys, webhook secret, callback origins, and audit identity. App private keys and webhook secrets stay in the operator's Hub secret manager and never ship with the desktop or official binaries.

## 3. Permissions

Default repository permissions:

| Permission | Level | Use |
|---|---|---|
| Metadata | Read | Repository identity |
| Contents | Read | Commit/tree/blob metadata and configuration |
| Contents | Write optional | Machine files/branches when enabled |
| Pull requests | Read/write | Proposal/release PRs and comments |
| Checks | Read/write | Build/art/release checks |
| Actions | Read optional | Workflow status display |
| Commit statuses | Read optional | Legacy status display |

Organization members, administration, workflows write, secrets, deployments, issues, and discussions are not requested unless a separately specified feature requires them.

The App MUST work in read-only mode when write permissions are absent.

## 4. Installation binding

Project binds to:

- GitHub base URL;
- repository numeric ID;
- owner/repo display cache;
- App installation ID;
- installation account;
- permission snapshot/version;
- webhook delivery scope.

Numeric repository ID is authoritative. Rename/transfer updates display/owner data after API verification. Installation removal marks integration revoked and stops API mutations.

## 5. User authorization

User access token proves actor identity for attributed actions. Installation token authorizes App operations. Hub never treats installation token as a human approval.

Desktop uses device/browser flow per SPEC-04. Self-hosted Hub stores App private key in secret manager and mints short-lived installation tokens server-side.

Private keys and webhook secrets never ship in desktop.

## 6. Token management

- Cache installation tokens only until provider expiry minus 60 seconds.
- Serialize token mint per installation.
- Store no installation token durably unless unavoidable; regenerate from App key.
- User tokens in OS keychain/Hub encrypted secret store.
- API client redacts all authorization headers.
- Revocation/installation suspension transitions integration immediately.

## 7. API use

Required operations, permission-dependent:

- get repository metadata/default branch;
- get refs, commits, trees, blobs/contents metadata;
- list/check pull requests and reviews;
- create branch/ref;
- create/update PR;
- create/update check runs;
- read workflow/check conclusions;
- optionally create commits through Git data API for small machine-authored files, though local Git push is preferred for cohesive changes.

Large native blobs do not pass through GitHub Contents API; they use Git/LFS/provider protocols.

## 8. Pull-request workflows

PR categories:

- code/build configuration;
- art pointer/revision proposal;
- production drift adoption;
- dependency/package update;
- generated release receipt/spec change.

PR body includes machine-readable hidden marker with project ID, proposal/build/release IDs, and immutable hashes. Marker is authenticated/validated; user-edited body text is not authority.

NeuMan never assumes a merged PR equals successful deployment. Git merge, art acceptance, build, and release remain separate states linked by evidence.

## 9. Checks

Reference check names:

- `NeuMan / Configuration`
- `NeuMan / Ownership`
- `NeuMan / Art Validation`
- `NeuMan / Build <place>`
- `NeuMan / Staging`
- `NeuMan / Release Readiness`

Check conclusions map:

- `success` gates passed;
- `failure` deterministic validation/build failure;
- `neutral` not applicable;
- `cancelled` user/system cancelled;
- `timed_out` deadline;
- `action_required` approval/manual Studio step;
- `skipped` policy skip.

Details URL points to authorized Hub or local handoff page; it MUST not leak private data to unauthorized viewers.

Check output stays within provider limits and summarizes with links to full logs. Native data is never attached.

## 10. Webhooks

Minimum subscribed events:

- installation / installation_repositories;
- push;
- pull_request;
- pull_request_review;
- check_suite / check_run as needed;
- workflow_run optional;
- repository rename/transfer/archive where available;
- ping.

### Validation

- HTTPS only in production;
- read raw request body with strict size limit;
- validate GitHub HMAC signature using current/previous rotating webhook secrets;
- require delivery ID/event headers;
- allowlist event names/content types;
- persist delivery ID and raw-body hash before async processing;
- acknowledge quickly after durable enqueue;
- duplicate delivery ID with same hash is idempotent;
- duplicate ID different hash is security alert.

Raw webhook retention is minimized/redacted per policy.

## 11. Event reconciliation

Webhooks are hints. On event, worker fetches authoritative API state using repository ID and ETags. Missing/out-of-order webhooks are repaired by periodic reconciliation:

- active PR/check state every 5–15 minutes while relevant;
- default branch head periodically and on app reconnect;
- installation/permission snapshot daily or on auth error.

## 12. Branch protection and rulesets

NeuMan respects GitHub branch protection/rulesets. It does not request bypass by default. If repository requires PR reviews, signed commits, checks, or linear history, generated workflow conforms or reports blocker.

Production release permission in NeuMan does not authorize bypassing GitHub rules.

## 13. Comments and annotations

PR comments use one updatable summary comment per NeuMan proposal where possible to avoid spam. Inline annotations only target stable repository file paths/lines; art cell semantic findings appear in check details unless mapped to committed metadata.

All rendered provider/user text is escaped. Bot comments contain no credentials/private object URLs.

## 14. Rate limiting

- Track REST/GraphQL rate headers per token/installation.
- Use conditional requests/ETags.
- Batch GraphQL only where query complexity and permissions are controlled.
- Respect secondary rate limits and `Retry-After`.
- Backoff with jitter; no aggressive polling.
- User-visible operation distinguishes queued for rate limit from failure.
- Production mutation does not wait indefinitely; approval can expire.

## 15. GitHub outage/offline

Local Git and cached work may continue. Unavailable GitHub App features:

- new PR/check creation;
- authoritative protected-branch/CI proof;
- App-based repository authorization.

Required GitHub proof gates remain blocked, not waived automatically.

## 16. GitHub Enterprise Server

Provider profile specifies API/web base URLs and supported version. TLS trust follows operator configuration with warnings for custom CAs. Feature detection controls checks/device flow/GraphQL availability. Public GitHub assumptions never apply silently.

## 17. Security

- App private key Hub-only secret manager.
- Webhook HMAC validation before parsing deep content.
- Repository ID/project binding on every event/action.
- Prevent confused-deputy cross-project installation use.
- Do not fetch arbitrary URLs from webhook payloads; construct allowlisted API requests.
- Sanitize Markdown and log fields.
- Audit App actions with actor attribution and request IDs.

## 18. Error codes

- `GHA_NOT_INSTALLED`
- `GHA_INSTALLATION_SUSPENDED`
- `GHA_PERMISSION_MISSING`
- `GHA_REPOSITORY_MISMATCH`
- `GHA_TOKEN_FAILED`
- `GHA_WEBHOOK_SIGNATURE_INVALID`
- `GHA_WEBHOOK_REPLAY_CONFLICT`
- `GHA_RATE_LIMITED`
- `GHA_RULESET_BLOCKED`
- `GHA_CHECK_FAILED`
- `GHA_API_UNAVAILABLE`

## 19. Acceptance criteria

1. Permission matrix tests prove read-only degradation and no hidden admin requirement.
2. Installation removal/transfer/rename reconcile correctly by repository ID.
3. Webhook signature/replay/fuzz tests pass.
4. Out-of-order/duplicate/missing webhooks converge through API reconciliation.
5. Checks reflect NeuMan state accurately and never imply deployment from Git merge alone.
6. Branch protection is never bypassed without separately approved policy.
7. Rate-limit tests prevent hot loops and preserve user-visible queued state.

## 20. References

External sources last verified: 2026-07-09.

- [GitHub App registration](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app)
- [GitHub App user access tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app)
