# NeuMan Studio Bridge

This distribution contains the authenticated local bridge and the corresponding
Roblox Studio plugin described by SPEC-07 and SPEC-08.

## Files

- `bridge.rs` is the embeddable daemon-side bridge implementation.
- `neuman_bridge.rs` is a diagnostic standalone host with explicit console
  approval. The desktop embeds the same library and presents approval in its UI.
- `schemas/bridge-protocol.schema.json` is the checked-in protocol 1.0 JSON Schema and
  includes conformance examples.
- `studio_plugin.luau` is the auditable, single-script Studio distribution.
- `studio.project.json` builds the script as a plugin with Rojo.

## Run the bridge

From this repository:

```powershell
cargo run --bin neuman-bridge
```

The process binds discovery to `127.0.0.1:34873` and its WebSocket to a random
`127.0.0.1` port. It refuses non-loopback listener configuration. Optional
development settings are `NEUMAN_BRIDGE_DISCOVERY_ADDR` and
`NEUMAN_BRIDGE_TRANSFER_DIR`; the address must remain numeric loopback.

When Studio discovers the bridge, the console prints a six-digit code and its
random challenge. After the plugin submits that code, the console prints the
exact approval command:

```text
approve <challenge> <plugin-id>
```

Approval is intentionally explicit. The plugin retries pairing and receives a
random 256-bit credential scoped only to this daemon installation, OS-local
bridge, and plugin installation. Revocation is immediate for future sessions:

```text
revoke <plugin-id>
```

The production desktop consumes `BridgeEvent` and invokes the same
`approve_pairing`/`revoke_plugin` API. It must never enable automatic approval.

## Build and install the Studio plugin

With a pinned Rojo toolchain:

```powershell
rojo build studio.project.json --output NeuManStudio.rbxm
```

Install `NeuManStudio.rbxm` through Studio's local plugin management UI. A
production package should be code-signed/checksummed by the release workflow
before distribution. The plugin never downloads or executes code.

Studio must permit HTTP requests. The plugin capability-checks
`SerializationService` and `HttpService:CreateWebStreamClient`; if either is
absent it remains read-only and explains the incompatibility in its widget.
The WebSocket uses the documented
`Enum.WebStreamClientType.WebSocket` request-options form and waits for the
`Opened` event before sending authentication or pairing data.

## Authoring workflow

1. Start the desktop/bridge and open the plugin widget.
2. Enter the project ID and authoring channel from `neuman.project.yaml`.
3. Pair once using the code shown by the desktop, approve the exact request, and
   reconnect. No Roblox, GitHub, Hub, object-store, or release token enters the
   plugin settings store.
4. Select an eligible Model/Folder and register it. This creates a UUIDv4
   `NeuManCellId` inside a Studio undo recording.
5. Edit the cell, then checkpoint it. The plugin waits for a stable mutation
   epoch, serializes with `SerializationService`, computes the canonical
   `b3-256:<lowercase-base32-no-padding>` BLAKE3 identity, and uploads
   bounded base64url chunks. The daemon quarantines and verifies the complete
   native blob before accepting a capture proposal.
6. Accepted incoming revisions appear without reopening Studio. Applies remain
   user-previewed: dirty same-cell state blocks them, all native roots stage
   detached, scripts/remotes are rejected, and replacement happens in one
   `ChangeHistoryService` recording. Any failure rolls back or enters an explicit
   recovery-required state.
7. Native cells that would exceed the 1 MiB control-message limit use
   `http-download-v1`: the authenticated WebSocket delivers a two-minute,
   session-scoped transfer capability; Studio downloads from the same numeric
   loopback authority with a one-purpose bearer header, then verifies exact size
   and BLAKE3 before deserialization. The token never enters a URL or setting.

## Security and operational limits

- Discovery and WebSocket URLs must use numeric `127.0.0.1`; hostnames,
  redirects, credentials in URLs, query strings, and non-loopback addresses are
  rejected.
- Discovery challenges expire after 30 seconds and are one-use. Five incorrect
  code attempts lock that plugin/challenge pair.
- Auth fallback permits only `session.authenticate` as the first frame and has a
  five-second timeout.
- Post-auth envelopes have exact monotonic sequences. Changed duplicate IDs,
  gaps, stale contexts, wrong project/place/channel, and protocol-major mismatch
  fail closed.
- Control messages are at most 1 MiB, chunks at most 256 KiB raw, and cells at
  most 96 MiB. Only one upload is active per Studio session. Large downloads are
  bounded to 96 MiB, expire after two minutes, permit at most three retry reads,
  and are removed when the Studio session disconnects. Partial uploads are
  removed on cancellation/disconnect.
- Every upload offer, chunk, completion, verified transfer, and capture is bound
  to the immutable project/workspace/place/channel context generation accepted
  at offer time. A workspace switch invalidates the transfer; a verified blob
  cannot be substituted for another cell ID. Accepted fan-out is filtered by
  the same per-session hello and context generation, not authentication alone.
- Native bytes are not logged and are not exposed to capture/apply logic before
  complete size/BLAKE3 verification.
- Capture and structural apply are disabled during play/test. Terrain and
  service-state mutation are visibly unsupported in this first plugin build.
  Cross-cell external-reference rewriting is also deferred; nested/crossing cell
  boundaries are rejected instead of being guessed.
- Apply commands select exactly one payload form per cell: bounded inline
  base64url or an authenticated `http-download-v1` descriptor. Both converge on
  the same size/hash check before detached native staging.

## Verification

Run the Rust bridge unit and integration checks with:

```powershell
cargo test --lib bridge
```

The suite covers non-loopback rejection, Host-header rebinding protection,
protocol negotiation, pairing approval and credential scoping, sequence gaps,
changed-message replay, chunk hashing, total hashing, quarantine, and limits.
Studio compatibility qualification still requires the golden native place corpus
on supported Windows/macOS Studio builds because native serialization and
WebStream behavior are engine capabilities, not reproducible in a Rust test.
