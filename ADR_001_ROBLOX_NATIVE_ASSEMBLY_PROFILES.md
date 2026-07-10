# ADR-001: Roblox native assembly execution profiles

Status: Accepted for implementation; provider behavior remains Phase-0 qualified  
Decision date: 2026-07-10  
Owners: NeuMan maintainers

## Context

NeuMan must combine filesystem-authored code with Studio-authored native Roblox instances without claiming that binary Roblox models can be safely text-merged. Three Roblox execution surfaces are relevant but do not have the same security capabilities:

1. A Studio plugin can call APIs whose security context is `PluginOrOpenCloud`, including `SerializationService`.
2. Open Cloud Luau Execution can run a bounded headless task against a selected place/version, accept binary inputs, use `SerializationService`, and optionally save through `AssetService:SavePlaceAsync()`. The current provider profile is API-key authenticated.
3. Studio CLI `RunScript` is documented to open a local or published place, execute a command-bar-level script, write output, and quit. That documentation does not grant `PluginOrOpenCloud` capabilities.

Treating these surfaces as interchangeable would create an unsafe publication claim and an implementation that can pass local mocks but fail in real Studio.

## Decision

NeuMan defines three explicit runner profiles.

### Public desktop: signed Studio plugin

The signed, paired Studio plugin is the native assembly authority for the open-source public desktop. It receives a short-lived authenticated manifest over numeric loopback, downloads exact content-addressed native cells, calls `SerializationService` in plugin context, applies changes inside Studio change history, validates the resulting managed state root, and returns an authenticated receipt. `SavePlaceAsync()` is a separately qualified and explicitly confirmed publish operation.

The desktop stores Roblox OAuth tokens only in the OS credential vault. Plugin pairing credentials are local, narrow, revocable credentials and are never Roblox OAuth tokens.

### Operator-owned CI: Open Cloud Luau Execution

A repository/operator MAY enable a high-fidelity CI profile using its own Roblox API key. CI submits a fixed runner and bounded binary inputs against an exact disposable or approved place/version, polls the task, validates the same logical manifest/receipt contract, and reconciles any lost response before retrying publication. The durable operator state machine reserves a mutation before dispatch; a crash or unknown upload/task-creation result becomes reconciliation-required instead of being retried blindly. The API key stays in the operator's CI secret store. NeuMan's public desktop, plugin, optional Hub, manifests, logs, and support bundles MUST NOT accept or store it.

### Studio CLI: launch and qualified validation

CLI `RunScript` MAY supervise Studio launch and run a fixed validation script using only APIs proven for that exact Studio build/platform/context. Its qualification report records both available and unavailable capabilities. It is not the v1 native RBXM deserialization authority and a successful CLI exit is not publication evidence.

## Authority and merge consequences

- Git commit plus pinned Rojo mapping is code authority.
- Accepted immutable art-cell revision is native art authority.
- A build resolves exact code, art, dependency, base, policy, and tool hashes before native assembly.
- Native cells are never line-merged. Concurrent edits produce separate proposed revisions; leases reduce collisions and explicit review chooses an accepted head.
- Staging and production promote the same immutable bundle. Neither path silently rebuilds from a moving branch or live Studio state.

## Security consequences

- Runner manifests expire within 15 minutes, include 256-bit nonces, exact cells, executor profile, target, validators, and a profile-compatible receipt channel, and are authenticated with a one-operation key.
- Studio-plugin manifests use numeric loopback. Operator manifests use a signed task return value and require exact universe/place/source-version binding.
- Receipts bind the executor profile, operation, exact cell hashes, validator implementation hashes, managed state root, provider task evidence where applicable, and exact publish target/version evidence.
- Replay of either operation ID or nonce fails closed.
- Team Create, target mismatch, missing permission proof, unresolved drift, unsupported native classes, or an unknown runner capability blocks mutation/publication.
- No NeuMan-operated central service or database is introduced by either profile.

## Qualification gates

The plugin profile cannot be called production-ready until `P0-03`, `P0-05`, `P0-07`, and the relevant compatibility corpus pass on supported Windows/macOS Studio builds. CLI behavior is governed by `P0-06`. The operator CI profile remains disabled until `P0-11` proves task binary inputs, fixed-runner behavior, timeout and lost-response reconciliation, signed receipts, `SavePlaceAsync()` version evidence, and API-key isolation.

## Rejected alternatives

- **Assume CLI `RunScript` can deserialize RBXM:** rejected because `SerializationService` is explicitly `PluginOrOpenCloud`.
- **Send an operator API key through desktop or Hub:** rejected because it violates the public-client/no-central-secret boundary.
- **Treat raw RBXM Git merges as semantic merges:** rejected because binary byte structure is not a stable collaborative merge model.
- **Make a proprietary VCS the authority:** rejected. Optional object-store adapters may be added, but open content-addressed cells, Git code authority, and exportability remain canonical.

## Reconsideration triggers

Revisit this ADR only if Roblox publishes a stable capability change with exact authentication, permission, execution-context, and place-version semantics. Any change requires a new compatibility corpus, threat-model review, migration plan, and replacement ADR; an undocumented successful experiment is insufficient.

## Primary references

- [Roblox SerializationService](https://create.roblox.com/docs/reference/engine/classes/SerializationService)
- [Roblox Studio command-line interface](https://create.roblox.com/docs/studio/command-line-interface)
- [Roblox Luau Execution](https://create.roblox.com/docs/cloud/reference/features/luau-execution)
- [Roblox AssetService SavePlaceAsync](https://create.roblox.com/docs/reference/engine/classes/AssetService#SavePlaceAsync)
