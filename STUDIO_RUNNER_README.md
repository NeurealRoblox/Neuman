# Fixed native runner and executor contract

`studio_runner.rs` implements the authenticated declarative boundary for Studio-assisted and operator-owned Open Cloud assembly, validation, and publication described by SPEC-13/14. `native_execution.rs` implements the profile-aware durable orchestration boundary. `runner_manifest.schema.json` is the public envelope schema.

The contract deliberately contains no Luau source, filesystem path to repository code, service/method name, arbitrary arguments, cloud credential, or bearer token. The fixed runner shipped in a signed NeuMan release supports only `assemble-validate`, `validate-only`, and `publish-validated`.

## Security properties

- The orchestrator generates a one-operation 256-bit key. The public profile delivers it out of band with the paired local runner capability; the operator adapter injects it only into the operator-owned task invocation.
- Manifest and receipt use RFC 8785/JCS bytes, distinct domain separators, and HMAC-SHA-256.
- Operation ID and 256-bit nonce are single-use; the in-process replay guard consumes both.
- Lifetime is positive and at most 15 minutes, with one minute maximum issue-time skew.
- The signed `executorProfile` cannot be substituted after dispatch. Studio plugin jobs require an exact numeric-loopback receipt channel; operator jobs require a task-return channel correlated to the operation ID.
- The signed manifest binds the fixed runner implementation SHA-256. Operator adapter startup and returned receipts must match it; the Studio job also binds the paired plugin implementation observed during session authentication.
- Operator Open Cloud jobs require exact universe, place, and immutable source-place-version IDs. Public Studio plugin jobs cannot claim Open Cloud source-version evidence.
- Native inputs are immutable BLAKE3 identities, exact sizes, RBXM media type, unique canonical DataModel slots, and strict aggregate bounds.
- Cells and validators are ordered and unique. Validators select fixed implementations by ID and source SHA-256.
- Studio publish manifests require exact universe/place IDs; local non-publish manifests reject them. Open Cloud manifests always require the exact provider target because even validation runs against a Roblox place version.
- A successful receipt must prove every exact applied cell, every declared validator, all blocking validators passing, and a final managed-state root.
- Open Cloud receipts additionally bind the provider execution session/task/source version. Successful publish receipts bind the profile-compatible `SavePlaceAsync` method and newly observed target version.
- Keys redact `Debug` output and overwrite their backing byte array on drop. Signature comparison is constant-time.

## Operator credential isolation

`OperatorOpenCloudJob`, `OpenCloudAction`, and `OperatorOpenCloudExecution` are safe to serialize and contain no API key field. `OperatorApiKey` implements neither `Clone` nor `Serialize`; it is accepted only by the operator-only `OperatorOpenCloudAdapter` call and zeroes its backing bytes on drop. The public desktop, Studio plugin, optional Hub, persisted job state, manifests, receipts, and logs never need the key.

The Open Cloud state machine reserves each action durably before dispatch. If a process dies with an upload or task-creation mutation in flight, recovery enters `reconciliation-required` and emits no duplicate mutation. A read-only task poll may be retried. Provider task failure, timeout, invalid receipt signature, source-version mismatch, and task-identity mismatch all fail closed.

## Still required for live qualification

This contract does not claim live provider feasibility by itself. The fixed Luau implementations and adapters still need the SPEC-20 Phase-0 corpus: plugin `SerializationService`, cross-cell reference resolution, terrain/service adapters, final artifact handling, `SavePlaceAsync` behavior, lost-response reconciliation, and Windows/macOS Studio-version qualification. Operator CI separately needs live binary-input/task/log polling and API-key-scope qualification against disposable version-pinned places. CLI `RunScript` remains a launch/qualified-validation surface, not the native deserialization authority. No build becomes native or publishable from a manifest alone; it requires a verified signed receipt from a qualified executor.
