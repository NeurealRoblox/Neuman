# SPEC-13 — Roblox Integration

Status: Draft; Studio-assisted publication requires Phase 0 proof  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-03, SPEC-04

## 1. Purpose

This specification defines supported Roblox authentication, API use, universe/place discovery and metadata, Studio installation/process integration, engine runner, native capture, publication modes, server restarts, drift evidence, rate limiting, and platform-policy boundaries.

## 2. Supported interfaces only

Allowed:

- Roblox OAuth 2.0/OIDC public-client PKCE;
- documented Open Cloud endpoints supporting OAuth;
- documented API-key endpoints only in operator-controlled CI outside public key collection;
- documented Roblox Studio plugin APIs;
- documented Studio command-line arguments;
- documented `AssetService:SavePlaceAsync` where enabled/supported;
- Creator Dashboard/manual handoff.

Forbidden in product:

- Roblox session cookie automation;
- undocumented CSRF/cookie endpoints;
- scraping Creator Dashboard;
- internal Studio CLI flags;
- credential interception;
- browser automation to publish or bypass consent.

## 3. OAuth

Implements SPEC-04. Current integration categorizes as Creation & Productivity. App registration/review requires official terms/privacy URLs and minimum scopes.

Identity uses OIDC `sub` Roblox user ID. Username/display name are mutable display cache.

## 4. OAuth scope plan

Baseline:

- `openid`
- `profile`
- `universe:read`

Feature-scoped additions may include allowed `universe:write`, `universe.place:write`, server restart, asset read/write, or other Creation & Productivity scopes only when exact endpoints and app policy permit.

Place publication via `/universes/v1/{universeId}/places/{placeId}/versions` is currently API-key-only; OAuth sign-in MUST NOT be represented as permission to use that endpoint.

## 5. Open Cloud client

- Base origins are hardcoded/allowlisted official Roblox HTTPS origins.
- OIDC discovery may update endpoints only within validated official origin policy.
- User-Agent identifies NeuMan version/platform where headers permit.
- Request correlation ID recorded, secrets redacted.
- JSON/body size and schema strictly validated.
- Roblox numeric IDs encoded exactly.
- Timeouts: connect 10 seconds, response 30 seconds default; upload/long operations explicit.
- Redirects disabled or restricted to official HTTPS allowlist, preserving no auth across origin.
- TLS uses OS trust; no certificate bypass.

## 6. Retry and rate handling

Retry only idempotent reads and mutations with documented idempotency/evidence strategy. Status classes:

- 401: one refresh then reauth;
- 403: permission/resource error, no blind retry;
- 404: target absent/mismatch;
- 409/412: conflict/stale state;
- 429: obey Retry-After/backoff;
- 5xx/network: bounded exponential backoff with jitter;
- unknown mutation response: reconcile external state before retry.

Provider request limits from headers/docs are enforced through token buckets. Studio/HTTP limits are handled separately.

## 7. Resource discovery

After OAuth:

1. fetch user info;
2. retrieve authorized resource information/token resources where applicable;
3. list/select accessible universes using supported APIs/UI search;
4. fetch `Get Universe` and `Get Place` for exact selected IDs;
5. verify creator ownership/type and user permissions;
6. store target identity and observation time;
7. require revalidation before high-impact mutations.

User-entered IDs are allowed only as lookup hints; API result establishes identity/access.

The implemented desktop provider uses `POST /oauth/v1/token/resources` to enumerate concrete universe targets, then stable OAuth `Get Universe` and `Get Place` reads. Roblox may return the owner-wide target value `U`, which grants access but does not enumerate universe IDs; in that case the UI accepts an exact numeric lookup hint and requires the provider read to succeed. The current Open Cloud schema has no stable OAuth List Places operation. NeuMan MAY use the documented unauthenticated `GET https://develop.roblox.com/v1/universes/{universeId}/places` index to discover candidate place IDs only after the universe is authorized, but MUST perform the stable OAuth Get Place read before the place becomes an authoritative selection. Index metadata alone is never access evidence.

The native desktop owns the OS-vault read and returns only typed metadata/evidence. Resource commands refresh a near-expiry access token through the protected rotating session, use pinned HTTPS origins, disable redirects, bound every response/pagination sequence, and clear inventory/selection on failure. See `ROBLOX_RESOURCE_PROVIDER.md`.

## 8. Universe operations

Supported when OAuth endpoint/scope allows:

- read display/config information;
- update explicitly supported metadata with diff/confirmation;
- restart servers with impact forecast where available;
- read restart status/server metadata/logs when authorized.

Every write shows current and desired values, target IDs, scope, and result receipt.

## 9. Place operations

Supported:

- read stable place metadata;
- update supported place metadata through OAuth when configured;
- open latest published/specific revision in Studio using documented CLI;
- capture managed roots through plugin/runner;
- publish through selected supported method.

The v1 desktop implementation exposes only the first item as read-only metadata. It contains no universe/place metadata mutation command, Place Publishing request, API-key field, generic provider request, or generic CLI mutation. A desktop release plan records intent only. Operator-owned CI is the only automated Place Publishing profile until a later reviewed ADR expands this boundary.

Open Cloud instance APIs are beta, collaborative-session, API-key-only for relevant script operations, and limited to scripts. The public product MUST NOT depend on them for full art sync or ask for a key.

## 10. Full-place/content pull

There is no assumed OAuth endpoint for downloading a complete private place file.

Supported capture path:

1. Studio CLI opens published place or revision using signed-in Studio account;
2. fixed NeuMan runner/plugin enumerates configured managed roots;
3. native cells/terrain/service state captured over loopback;
4. daemon validates and creates observed/adoption proposal;
5. no automatic accepted-head/main update.

Manual `Download a Copy` may be imported with explicit provenance but is not automated through unsupported APIs.

## 11. Studio discovery

Windows default search:

- Roblox version installations beneath documented local app data location;
- validate executable signature/publisher, file version, and executable name;
- do not choose solely by newest directory timestamp.

macOS default:

- `/Applications/RobloxStudio.app/Contents/MacOS/RobloxStudio` and operator-approved alternate app location;
- validate code signature/bundle ID/version.

User override requires file picker and validation. Installation record contains path, platform, channel/build, signature status, last used.

## 12. Studio launch

Only documented commands:

- `EditPlace`
- `EditPlaceRevision`
- `EditFile`
- `RunScript`
- documented script/open/highlight arguments
- API dump arguments for compatibility generation.

Arguments are arrays, never shell. IDs/paths revalidated. NeuMan does not attach to or terminate unrelated Studio instances.

## 13. Engine runner

Runner is a fixed, versioned Luau program shipped/signed with NeuMan. It MUST NOT read/execute Luau from repository, Hub, or provider. The signed declarative manifest and receipt contract is implemented in `studio_runner.rs` and `runner_manifest.schema.json`; profile dispatch, durable Open Cloud action state, and the operator-only credential adapter boundary are implemented in `native_execution.rs`.

Execution profiles are capability-specific:

- **Studio plugin profile (public desktop default):** the installed NeuMan plugin owns `SerializationService` access, receives exact native inputs through the authenticated loopback bridge, applies them inside Studio undo history, and returns the signed receipt to the local daemon.
- **Open Cloud Luau Execution profile (operator-owned CI):** a fixed runner executes headlessly with operator-managed API-key scopes, task binary inputs, `SerializationService`, and optionally `SavePlaceAsync`. The public desktop/Hub never receives the key.
- **Studio CLI `RunScript` profile:** documented for opening a place, executing a fixed command-bar-level script, writing output, and exiting. It MAY perform validation available to that context, but it MUST NOT be assumed to have `PluginOrOpenCloud` capabilities such as `SerializationService` until a current Studio qualification proves the exact call. It is not the v1 native-deserialization authority.

Invocation input:

- local candidate place or exact published target;
- operation-specific runner file path;
- loopback endpoint plus one-time capability token delivered without command-line secret when possible (inherited protected file/handshake); if an argument is unavoidable in Phase 0, token is short-lived/non-cloud and process diagnostics redact it;
- output log path in operation directory;
- `--quitAfterExecution` for non-interactive runs.

Runner protocol:

1. authenticate to the local daemon or operator-owned Open Cloud task boundary;
2. report engine/user/place/capabilities;
3. receive signed declarative operation manifest;
4. fetch hash-addressed native content;
5. assemble/validate/capture/publish only predefined operations;
6. send structured result receipt;
7. exit with explicit success/failure marker.

Runner validates project, operation, expiry, content hashes, target constraints, and allowed operation. It rejects arbitrary method names/code.

## 14. Studio-assisted assembly

1. Rojo/core creates local candidate with code/declarative content/art slots.
2. Studio opens the candidate with the signed NeuMan plugin active; CLI `EditFile`/`RunScript` MAY assist launch/validation but does not grant native deserialization authority.
3. The plugin validates the signed declarative runner manifest and removes/replaces only generated art slots.
4. Plugin-context `SerializationService` deserializes native cells.
5. Terrain/service adapters apply.
6. Cross-cell references resolve.
7. validation suite runs.
8. runner produces logical equivalence receipt.

Candidate remains local until explicit publish stage.

## 15. Publication mode A: Studio-assisted

Preferred public-safe high-fidelity path, contingent on Phase 0.

Preconditions:

- exact target place API enabled for `SavePlaceAsync` as Roblox requires;
- Studio signed-in user has permission;
- no active Team Create session for current local candidate;
- release bundle/approval valid;
- engine-side validation passed;
- target/drift/predecessor revalidated;
- user confirms production impact.

Action:

```luau
AssetService:SavePlaceAsync({
  PlaceId = targetPlaceId,
  SaveWithoutPublish = false,
})
```

The actual runner uses strict manifest values, protected call, timeout/evidence, and post-publish observation. `SaveWithoutPublish=true` is used only for explicit saved-draft workflow.

Restrictions:

- active Team Create blocks saves;
- simultaneous saves are externally ordered, so predecessor/drift checks are required;
- API errors may be ambiguous; reconcile before retry;
- Phase 0 determines interaction/prompt behavior per platform/account.

## 16. Publication mode B: Open Cloud

Only operator-controlled CI or approved deployment configuration supplies API key outside public app.

Endpoint uploads `.rbxl/.rbxlx` with version type and returns version number. Preconditions:

- artifact compatibility scanner proves no unsupported modified instance types for this method;
- target key scoped to exact universe/write and stored in secret manager;
- artifact size/content type valid; the current documented place-file ceiling is 100 MiB (104,857,600 bytes), and the implementation MUST use the lower of the documented limit and any provider response limit observed during qualification;
- target/predecessor/release gates valid.

Known official limitation set includes modified EditableImage, EditableMesh, PartOperation, SurfaceAppearance, and BaseWrap requiring Studio publication. Scanner is versioned and fail-closed for unknown risky types.

Desktop/Hub may orchestrate CI job status but never receive raw key.

### 16.1 Operator CI Luau Execution subprofile

Roblox's stable Luau Execution APIs MAY be used by operator-owned CI for high-fidelity assembly, testing, and publication. The profile:

1. uploads each exact RBXM as a task binary input scoped to the target universe;
2. submits the fixed NeuMan Luau source plus immutable manifest references to the exact universe/place or place version;
3. polls the returned execution-session/task resource with bounded backoff for at most the declared operation timeout;
4. retrieves structured results/logs, verifies the signed receipt and every applied cell/validator, and records the provider resource path;
5. uses `SavePlaceAsync` only when the release manifest explicitly selects publication and all predecessor/approval/target gates were revalidated immediately before task creation.

The task has a finite provider lifetime and concurrency quota. A timeout or lost response is `unknown` until the task resource and place-version evidence are reconciled; it MUST NOT trigger a blind duplicate publish. API-key scopes are limited to the required universe/place Luau-execution and publication operations and remain exclusively in the operator's secret manager.

Persisted jobs/actions contain the signed manifest, fixed runner hash, exact source-place-version target, immutable cell metadata, and non-secret provider IDs only. The API key is a non-serializable input to the operator HTTP adapter and is never accepted by desktop/plugin/Hub command schemas. The state machine records an action as in flight before the adapter executes it. Process recovery retries an in-flight read-only poll, but an in-flight upload or task creation becomes reconciliation-required and emits no further mutation until the operator adapter proves the provider state.

## 17. Publication mode C: manual handoff

NeuMan opens validated candidate in Studio and presents exact target/instructions. User publishes through Roblox UI. Afterward NeuMan captures/observes version and requires user confirmation/evidence before marking deployment. Manual does not mean unaudited.

## 18. Publication receipts

Receipt includes:

- method;
- target IDs;
- logical build/bundle hash;
- actor identity evidence;
- start/end time;
- predecessor observation;
- returned/observed version number;
- redacted provider result hash;
- validation receipt;
- ambiguous/reconciled flag;
- correlation ID.

No release is `published` without a version/evidence confidence allowed by policy.

## 19. Version history and drift

Roblox version history is a platform backup/evidence source. Experimental place-version-history endpoints are not a sole critical dependency. Drift evidence may come from:

- publication receipt/version;
- stable metadata APIs;
- experimental version history when operator credentials permit;
- Studio capture of managed content;
- generated release marker.

Confidence defined in SPEC-02. Unknown remains blocking if policy requires no unknown drift.

## 20. Server restart

Optional post-publish action:

1. fetch restart forecast/status when supported;
2. show expected player/server impact;
3. require release policy approval;
4. invoke OAuth-authorized restart endpoint;
5. poll operation/status with rate limits;
6. record affected versions/evidence;
7. failure does not change the published place version but makes the release `published-with-warning` when policy permits or `verification-failed` when restart verification is a required gate.

## 21. Multi-place constraints

Roblox provides no atomic multi-place publication. Release orchestrator handles order/compensation. Integration returns per-place commit-point and evidence; it never masks partial success.

## 22. Compliance

- App name/UI must not imply Roblox endorsement.
- Terms/privacy and user consent meet third-party app requirements.
- Request minimum scopes.
- No user profiling or data sale/training.
- No API-key requests from users.
- Respect app review/quota/rate adjustments.
- Provide revocation/data deletion behavior.

## 23. Error codes

- `RBX_OAUTH_REAUTH_REQUIRED`
- `RBX_SCOPE_MISSING`
- `RBX_RESOURCE_NOT_AUTHORIZED`
- `RBX_TARGET_MISMATCH`
- `RBX_API_RATE_LIMITED`
- `RBX_API_UNAVAILABLE`
- `RBX_STUDIO_NOT_FOUND`
- `RBX_STUDIO_SIGNATURE_INVALID`
- `RBX_STUDIO_VERSION_UNAPPROVED`
- `RBX_RUNNER_FAILED`
- `RBX_RUNNER_RESULT_AMBIGUOUS`
- `RBX_TEAM_CREATE_SAVE_BLOCKED`
- `RBX_SAVE_PLACE_NOT_ENABLED`
- `RBX_PUBLISH_METHOD_INCOMPATIBLE`
- `RBX_PUBLISH_PERMISSION_DENIED`
- `RBX_PUBLISH_RESULT_UNKNOWN`
- `RBX_RESTART_FAILED`

## 24. Acceptance criteria

1. OAuth public-client conformance passes on Windows/macOS.
2. Resource discovery validates user/group universe/place permissions.
3. No public path accepts Roblox cookie/API key.
4. Studio discovery validates signatures and documented arguments only.
5. Runner cannot execute repository/Hub Luau, rejects expired/wrong-target/replayed manifests, and produces a manifest-bound signed receipt.
6. Plugin-assisted Studio publication spike proves `SerializationService`, target, Team Create restriction, error reconciliation, and version evidence on disposable places; CLI `RunScript` capability is tested separately.
7. Open Cloud scanner blocks every documented unsupported modified instance class and unknown risky schema.
8. Ambiguous mutation response never causes blind duplicate publish.
9. Operator-owned Luau Execution proves binary input, task polling, signed receipt, timeout/reconciliation, and `SavePlaceAsync` behavior without exposing the API key to desktop/Hub.
10. Restart flow reports impact/status without changing publication evidence.

## 25. References

External sources last verified: 2026-07-10.

- [Roblox OAuth overview](https://create.roblox.com/docs/cloud/auth/oauth2-overview)
- [Roblox Open Cloud API index](https://create.roblox.com/docs/cloud/reference/domains/apis)
- [Roblox Place Publishing guide](https://create.roblox.com/docs/cloud/guides/usage-place-publishing)
- [Roblox Studio CLI](https://create.roblox.com/docs/studio/command-line-interface)
- [Roblox Luau Execution](https://create.roblox.com/docs/cloud/reference/features/luau-execution)
- [Roblox SerializationService](https://create.roblox.com/docs/reference/engine/classes/SerializationService)
- [Roblox AssetService](https://create.roblox.com/docs/reference/engine/classes/AssetService)
- [Roblox place files](https://create.roblox.com/docs/projects/place-files)
