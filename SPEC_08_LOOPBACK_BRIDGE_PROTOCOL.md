# SPEC-08 — Loopback Bridge Protocol

Status: Draft for Phase 0  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-02, SPEC-04

## 1. Purpose

The Loopback Bridge Protocol (LBP) connects the Roblox Studio plugin to the local NeuMan daemon for discovery, pairing, session state, commands, events, and chunked native content transfer.

LBP is local-only. It is not the Hub protocol and MUST NOT be exposed to LAN or internet interfaces.

## 2. Transport profile

Phase 0 preferred profile:

- discovery: HTTP/1.1 on `127.0.0.1:34873` and optionally `[::1]:34873`;
- session: WebSocket returned by discovery, numeric loopback host only;
- control/data: UTF-8 JSON messages;
- binary payload: base64url-no-padding chunks inside JSON for consistent Roblox WebStream behavior;
- one WebSocket per Studio plugin instance.

If Phase 0 proves reliable binary WebSocket frames, a `binary-chunks` negotiated minor profile MAY be added. JSON/base64 remains required fallback for v1.

Daemon MUST bind loopback explicitly, never `0.0.0.0`/`::`.

## 3. Discovery

Endpoint:

```http
GET /.well-known/neuman-studio-bridge HTTP/1.1
Host: 127.0.0.1:34873
Accept: application/json
```

Response:

```json
{
  "schemaVersion": "1.0",
  "installationId": "ins_<uuid>",
  "daemonVersion": "0.1.0",
  "protocolMin": "1.0",
  "protocolMax": "1.0",
  "webSocketUrl": "ws://127.0.0.1:49152/v1/studio",
  "pairingRequired": true,
  "challenge": "base64url-random",
  "expiresAt": "2026-07-09T22:14:33.127Z"
}
```

Rules:

- response header `Content-Type: application/json`;
- `Cache-Control: no-store`;
- `challenge` has at least 128 bits entropy and one use/30-second expiry;
- plugin rejects hostname, non-loopback IP, query/userinfo/fragment, or unexpected scheme;
- daemon validates `Host` against numeric loopback allowlist to mitigate DNS rebinding;
- discovery reveals no account/project data.

If port is unavailable due another owner, daemon fails health startup rather than choosing an undiscoverable silent port. Enterprise configuration MAY change discovery port in both daemon and managed plugin setting.

## 4. Pairing

### 4.1 User experience

For first pairing:

1. desktop displays six-digit code and plugin/Studio identity request;
2. plugin displays daemon installation name/fingerprint and asks user to enter/confirm code;
3. plugin sends pairing request over WebSocket using discovery challenge;
4. desktop user approves Studio installation/user context;
5. daemon issues opaque renewable pairing credential scoped to plugin installation and OS user.

Six-digit code is an interaction confirmation, not the sole cryptographic secret. Security comes from loopback, random challenge, daemon approval, and issued random credential.

### 4.2 Pair request

```json
{
  "type": "pair.request",
  "requestId": "<uuidv7>",
  "protocolVersion": "1.0",
  "challenge": "...",
  "pairingCode": "123456",
  "pluginInstallationId": "plg_<uuidv4>",
  "pluginVersion": "0.1.0",
  "studio": {
    "userId": "123",
    "version": "...",
    "platform": "windows"
  }
}
```

Pair success returns credential, installation ID, expiry/renewal policy, negotiated protocol. Credential has at least 256 random bits. Plugin stores it through `Plugin:SetSetting`; because this store is not a high-security vault, credential authorizes only this local bridge and can be revoked from desktop.

Pair failures use constant-time code comparison and rate limit: five failures per plugin installation/challenge, then new challenge/user action.

## 5. WebSocket authentication

On reconnect, preferred request header:

```text
Authorization: NeuManPair <credential>
X-NeuMan-Plugin-Installation: <id>
X-NeuMan-Protocol: 1.0
```

If Roblox WebStream cannot reliably set headers, negotiated fallback sends credential only in the first `session.authenticate` message over loopback; daemon permits no other message beforehand and closes on timeout after five seconds. Credential MUST never appear in URL.

Each connection receives an ephemeral `sessionToken` after authentication. Subsequent messages include a MAC only if Phase 0 shows value beyond WebSocket connection/session isolation; sequence and connection binding are mandatory either way.

## 6. Version negotiation

Client sends supported min/max. Server selects highest mutually supported minor within same major. No common major closes with `4406 PROTOCOL_INCOMPATIBLE` before project data exchange.

Capabilities are separately negotiated:

- `base64-chunks`
- `binary-chunks`
- `zstd-buffer`
- `terrain-region-rbxm`
- `terrain-voxel-zstd`
- `semantic-fingerprint-v1`
- `apply-transaction-v1`

Unknown capabilities are ignored. A command requiring absent capability is rejected before transfer/mutation.

## 7. Message envelope

Every post-auth JSON message:

```json
{
  "protocolVersion": "1.0",
  "type": "session.hello",
  "messageId": "<uuidv7>",
  "correlationId": "<uuidv7>",
  "sequence": 1,
  "sentAt": "2026-07-09T22:14:03.127Z",
  "sessionId": "ses_...",
  "payload": {}
}
```

Rules:

- `sequence` starts at 1 per direction and increases exactly by one;
- duplicate `messageId` with identical hash may be acknowledged idempotently;
- gap causes `protocol.sequence-gap` and resynchronization/close;
- duplicate ID with different content is security error/close;
- maximum clock skew warning 5 minutes; ordering relies on sequence, not timestamp;
- unknown message type gets typed unsupported response unless security critical.

## 8. Session hello/context

Plugin `session.hello` payload:

- plugin/Studio/capability report;
- user ID;
- universe/place/game IDs;
- DataModel name;
- run state;
- plugin installation ID;
- last known workspace/project/channel/config hash;
- resume token/cursors if any.

Daemon `session.context`:

- daemon/session IDs;
- project/workspace/place/channel binding or unbound reason;
- resolved ownership hash;
- policy hash;
- allowed command list;
- accepted art head/state root;
- heartbeat and transfer limits;
- compatibility result.

Context changes require new `contextVersion`. Every mutation command includes expected version; stale commands fail.

## 9. Heartbeat and liveness

- daemon sends `session.ping` every 10 seconds idle;
- plugin responds `session.pong` within 10 seconds;
- three missed heartbeats close connection and mark session disconnected;
- active large transfer frames count as traffic but do not replace explicit heartbeat for more than 30 seconds;
- WebSocket reconnect uses exponential backoff 0.5, 1, 2, 4, 8 seconds capped at 15 with jitter;
- pairing credential remains valid until revoked/rotated; session context must be revalidated after reconnect.

## 10. Command pattern

Daemon-to-plugin mutation:

```json
{
  "type": "command.request",
  "payload": {
    "commandId": "op_...",
    "command": "art.apply",
    "contextVersion": 4,
    "idempotencyKey": "...",
    "deadlineAt": "...",
    "arguments": {}
  }
}
```

Plugin responds:

- `command.accepted`
- streamed `command.progress`
- terminal `command.succeeded | command.failed | command.cancelled`

Acceptance means queued/preflight started, not mutation success.

Plugin-to-daemon requests use the same request/progress/terminal model with reversed direction.

## 11. Required message types

Session:

- `session.authenticate`
- `session.hello`
- `session.context`
- `session.context-changed`
- `session.ping`
- `session.pong`
- `session.goodbye`

Status/events:

- `studio.selection-changed`
- `studio.run-state-changed`
- `studio.place-context-changed`
- `art.cell-dirty-changed`
- `art.lock-status-changed`
- `art.incoming-summary`
- `diagnostic.event`

Commands:

- `art.capture`
- `art.apply`
- `art.register-cell`
- `art.resolve-conflict`
- `terrain.capture`
- `terrain.apply`
- `service.capture`
- `service.apply`
- `validation.run`
- `session.refresh-context`

Transfer:

- `transfer.offer`
- `transfer.accept`
- `transfer.chunk`
- `transfer.ack`
- `transfer.complete`
- `transfer.verified`
- `transfer.cancel`

## 12. Transfer protocol

### 12.1 Offer

```json
{
  "transferId": "op_...",
  "direction": "upload",
  "purpose": "art-cell",
  "resourceId": "cell_...",
  "contentHash": "b3-256:...",
  "mediaType": "application/x-roblox-rbxm",
  "sizeBytes": 10485760,
  "chunkSizeBytes": 262144,
  "chunkCount": 40,
  "encoding": "base64url",
  "compression": "none"
}
```

Receiver validates context, size, capability, quota and returns accepted chunk window or rejects before payload.

### 12.2 Chunks

```json
{
  "transferId": "op_...",
  "chunkIndex": 0,
  "offsetBytes": 0,
  "rawSizeBytes": 262144,
  "chunkHash": "b3-256:...",
  "data": "base64url-no-padding"
}
```

Rules:

- default raw chunk 256 KiB;
- chunk index zero-based;
- chunk hash covers decoded/decompressed raw chunk;
- receiver maintains bounded window default 4 chunks;
- ACK reports highest contiguous plus missing indexes;
- sender retries missing chunks up to 3 then fails;
- receiver assembles in declared order and verifies total content hash/size;
- content is not exposed to mutation logic until `transfer.verified`;
- cancel deletes partial buffers and releases quotas.

Plugin memory limits require daemon flow control. Daemon may spool uploads to a temporary file; plugin holds only bounded chunks plus engine buffer.

### 12.3 Compression

`none` is required. `zstd` is optional only when both peers report `zstd-buffer`. Compression is applied to complete content before chunking; offer includes uncompressed and encoded size/hash. Zip bombs are prevented by declared size and maximum decompressed buffer checks.

## 13. Idempotency and resume

- Capture command retry with same idempotency key and unchanged cell epoch returns existing draft/transfer result.
- Apply command retry queries prior receipt; it does not apply twice.
- Transfers MAY resume within same daemon/plugin session using received bitmap.
- Resume across Studio restart is not assumed because plugin buffer is gone; transfer restarts but CAS dedup avoids durable duplicate.
- A daemon restart reconciles command receipts before retrying apply.

## 14. Error envelope

```json
{
  "code": "LBP_SEQUENCE_GAP",
  "category": "conflict",
  "message": "Bridge message sequence is incomplete.",
  "retryable": true,
  "details": {"expected": 8, "received": 10},
  "correlationId": "..."
}
```

Required protocol errors:

- `LBP_DISCOVERY_INVALID`
- `LBP_PAIRING_REQUIRED`
- `LBP_PAIRING_DENIED`
- `LBP_AUTH_INVALID`
- `LBP_PROTOCOL_INCOMPATIBLE`
- `LBP_CONTEXT_STALE`
- `LBP_SEQUENCE_GAP`
- `LBP_REPLAY_DETECTED`
- `LBP_MESSAGE_TOO_LARGE`
- `LBP_TRANSFER_REJECTED`
- `LBP_CHUNK_HASH_MISMATCH`
- `LBP_CONTENT_HASH_MISMATCH`
- `LBP_DEADLINE_EXCEEDED`
- `LBP_COMMAND_UNSUPPORTED`

## 15. Close codes

- `1000` normal
- `1008` generic policy violation
- `4001` authentication required/invalid
- `4003` pairing revoked
- `4004` context not found
- `4009` sequence/replay violation
- `4010` message/transfer limit
- `4406` incompatible protocol
- `4500` internal bridge failure

Reconnect guidance is supplied in final error when possible.

## 16. Limits

Defaults:

- discovery body 16 KiB;
- control message 1 MiB;
- transfer chunk encoded message 512 KiB target;
- concurrent transfers per plugin: 1 upload and 1 download, but no simultaneous Studio serialization/apply;
- hard transfer content default 96 MiB for a cell;
- commands in flight 32;
- malformed messages before close: 1 for auth/security, 3 for non-security schema errors;
- pairing transaction 2 minutes;
- command default deadline 5 minutes, explicit longer for terrain/build validation.

Limits are sent in context and may be stricter. Sender obeys receiver.

## 17. Security requirements

- Numeric loopback only.
- Host header validation.
- Challenge/code rate limits.
- Pair credential never in URL/log.
- Credential scoped to OS user/daemon installation/plugin installation.
- Every mutation bound to context version/project/place/channel.
- Monotonic sequence and duplicate-content check.
- Size/hash verification before deserialization.
- No generic remote method invocation or arbitrary Luau.
- Daemon displays/revokes paired installations.
- Native payloads never included in diagnostic event by default.

## 18. Conformance and acceptance

1. Independent protocol fixture client/server pass golden messages.
2. Windows and macOS Studio probes validate discovery/WebSocket/header behavior.
3. Property tests cover chunk reordering, loss, duplication, corruption, cancellation, and limit enforcement.
4. Replay and stale-context commands cannot mutate Studio.
5. Daemon restart and plugin reconnect do not double-apply.
6. 96 MiB transfer completes within defined memory bounds on baseline hardware.
7. Non-loopback exposure tests fail closed.
8. Fuzzed JSON/base64 cannot crash either peer or allocate beyond limit.

