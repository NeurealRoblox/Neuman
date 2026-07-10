# Self-hosted Hub desktop adapter

The desktop can relay accepted art between machines through a Hub owned and
operated by the user or team. NeuMan does not provide a hosted endpoint or
central database.

## Trust and configuration

- `providers.hub` declares only `{ type: neuman-hub, url, projectId }` and may
  not contain a credential.
- The native process connects only when the exact declaration is repeated in
  the startup-only `NEUMAN_HUB_URL`. A project file or renderer command cannot
  redirect an active process. Public endpoints require HTTPS; HTTP is accepted
  only for numeric loopback.
- The bearer is read from the OS credential vault account derived from the
  normalized endpoint and project ID. There is no environment, file, SQLite,
  renderer, plugin, log, or URL fallback. Native provisioning uses
  `provision_hub_bearer`; it is deliberately not a Tauri command.
- Redirects are disabled. Hub-supplied upload/download URLs must retain the
  exact scheme, host, port, and transfer path. JSON and object responses are
  bounded before use.

## Outbound capture

After the loopback bridge has verified and durably committed a Studio capture,
the adapter materializes the complete local art state from CAS. It:

1. reads the remote project's default art channel and current head;
2. negotiates and hash-verifies immutable cell uploads;
3. acquires leases for the exact changed cell IDs using the remote head as base;
4. uploads one JCS canonical full-state manifest;
5. creates a proposal with deterministic idempotency keys.

The full manifest binds every `cellId` to its escaped DataModel parent path,
BLAKE3 content hash, exact byte length, base head/root, source Studio session,
and computed state root. Full snapshots permit recovery after event retention;
`changedCellIds` remains a delta so unchanged cells do not require leases.

Hub does not trust the proposal's independent arrays. In the proposal
transaction it loads the verified manifest object, requires canonical bytes,
recomputes the domain state root, compares the complete state with the accepted
base, derives the changed set, verifies every cell object and size, and requires
the request's resources and objects to be exact. Omission, extra-object,
forged-root, slot/content substitution, and unsupported deletion fail before
proposal insertion. Acceptance repeats validation and releases consumed leases.

## Inbound accepted art

The adapter polls the authenticated durable event API with its signed cursor.
Delivery is at least once. For `art.channel.head_changed`, it fetches the
revision, independently recomputes the state root, downloads bounded cells with
scoped transfer tokens, verifies size/BLAKE3, and imports the revision into the
local CAS and SQLite ledger before cursor advancement.

The local ledger maps `(Hub authority, Hub revision)` to one local immutable art
revision and advances the accepted head with compare-and-swap. Duplicate events
are harmless. A stale base stops normal delivery. If retention expires, the
cursor is cleared and retained accepted events are replayed as full snapshots.

Only then does the adapter call the existing bridge incoming/apply path. The
bridge requires the exact local project, channel, place/universe hello, and
workspace context generation. The originating Studio session is excluded; a
same-ID replay on another machine is not trusted. Workspace changes first clear
the binding, swap the native workspace under one gate, then install the new
binding and adapter.

## Bounds and retry policy

- at most 128 cells and 96 MiB total native bytes per accepted snapshot;
- at most 1 MiB per JSON/event response;
- HTTP connect/operation timeouts of 5/30 seconds;
- three idempotent HTTP attempts with 200/400/800 ms delay;
- event polling at one second when idle and exponential reconnect delay capped
  at 32 seconds;
- cursor advances only after CAS verification, durable local import, and bridge
  context/fan-out success.

The current transport uses the Hub's durable HTTP event page. The Hub WebSocket
gateway remains protocol-compatible and can replace polling as a latency
optimization without changing cursor, validation, or commit semantics.
