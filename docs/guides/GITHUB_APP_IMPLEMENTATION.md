# NeuMan GitHub App integration

`github_app.rs` is the production-shaped SPEC-12 integration boundary. It is
intentionally a library module rather than a route handler so the Hub can apply
its own authorization, audit, transactional outbox, and secret-manager policy.

The official NeuMan project does not operate a shared GitHub App backend. Each self-hosted Hub operator registers and owns its App, webhook endpoint, private key, secrets, installation grants, database, and audit trail. The desktop never receives an App private key or installation token.

## Implemented contracts

- GitHub webhook HMAC-SHA-256 verification operates on the exact bounded raw
  body before JSON parsing. It accepts a current and one overlap-period previous
  secret, uses constant-time digest comparison, requires delivery/event/content
  type envelope values, and enforces an event allowlist.
- `DeliveryReplayStore` requires an atomic reserve-or-compare operation. An
  identical delivery is idempotent; reuse of a delivery ID with different bytes
  becomes `GHA_WEBHOOK_REPLAY_CONFLICT`. The in-memory implementation is for
  tests/development only; Hub must back the trait with its durable unique-key
  transaction before returning a 2xx webhook acknowledgement.
- App, installation, and repository IDs are non-zero typed values.
  `RepositoryBinding` makes the numeric repository ID authoritative. Every API
  mutation verifies token, binding, and repository identity together.
- `AppJwt::issue` produces a short-lived RS256 JWT from private-key bytes passed
  by the caller. Private keys, webhook keys, App JWTs, and installation tokens
  redact debug output. The module has no embedded keys and no PAT path.
- Installation-token exchange asks GitHub for exactly one `repository_ids`
  scope. Tokens are returned in memory with provider expiry and should be cached
  no later than expiry minus 60 seconds.
- The provider client constructs endpoints locally on the exact
  `https://api.github.com` origin. Both the client and production transport
  enforce the origin, redirects are disabled, response size is bounded, and no
  webhook-supplied URL is fetched.
- Repository metadata is fetched through `/repositories/{numeric-id}` and the
  returned numeric ID is checked before owner/name becomes usable for check-run
  paths. Archived repositories reject mutations.
- Completed check-run create/update supports the SPEC-12 conclusion vocabulary,
  bounded output, fixed NeuMan check-name families, and HTTPS details links.
- Response classification distinguishes primary/secondary rate limiting,
  reauthentication, transient outages, and permanent failures. It does not
  silently retry mutations.
- PR hidden markers use canonical RFC 8785 JSON plus HMAC-SHA-256 in an exact
  HTML comment envelope. They bind project, numeric repository, optional
  proposal/build/release IDs, and sorted immutable hashes. Validation rejects
  tampering, non-canonical payloads, duplicate hashes/order, unknown fields, and
  multiple markers in one PR body.

## Hub wiring requirements

1. Load App private keys, webhook secrets, and marker keys from a platform secret
   manager. Never serialize them into project configuration, SQLite, logs, crash
   reports, desktop bundles, or Studio settings.
2. Put a SQL implementation behind `DeliveryReplayStore`. Insert delivery ID,
   body SHA-256, event, receive time, and processing state in one transaction,
   then enqueue through the Hub outbox before acknowledging. Retain or redact raw
   bodies according to policy.
3. Serialize token minting per installation and cache only in memory until the
   earlier of provider expiry minus 60 seconds or installation revocation.
4. Resolve the project binding from trusted Hub routing/auth context, not webhook
   owner/name. Re-fetch repository and installation/permission state after
   installation changes, authorization errors, rename/transfer signals, and on
   the SPEC-12 reconciliation schedule.
5. Attribute human approvals to a distinct authenticated NeuMan principal. An
   installation token proves App authority only and never counts as approval.
6. Feed retry classification into the durable queue with capped exponential
   backoff and jitter. Automatically retry only operations whose idempotency is
   proven; check-run callers should persist provider IDs and external IDs.

## Deliberate gaps

- GitHub Enterprise Server needs a separate operator-configured provider profile,
  custom-CA policy, feature detection, and an independently constrained origin
  allowlist. This module fails closed to public `api.github.com` and must not be
  loosened by accepting arbitrary base URLs.
- User browser/device authorization is a separate desktop identity flow. It is
  not implemented here and must not be substituted with PAT entry.
- PR/ref/comment operations, branch/ruleset inspection, ETag reconciliation,
  GraphQL batching, and Git/LFS transport belong to their dedicated workers.
- Secret zeroization here is best-effort safe-Rust hygiene, not a replacement for
  locked memory, process isolation, OS keychains, or a Hub secret manager.
- Live GitHub qualification still requires a development GitHub App, disposable
  repository, installation removal/suspension tests, permission-degradation
  tests, rate-limit drills, and webhook redelivery/out-of-order reconciliation.

## Verification

Focused unit tests cover the GitHub-published HMAC vector, signature-before-JSON
behavior, replay idempotency/conflict, SSRF-shaped URL rejection, marker
round-trip/tampering, numeric repository-scoped token requests, repository
verification, check-run request construction, authorization redaction, and
rate-limit classification through an injectable mock transport.
