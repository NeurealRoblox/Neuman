# Desktop Rojo configuration adapter

`rojo_desktop_config.rs` is the trust boundary between a desktop workspace selection and `RojoSessionManager`.

## Webview contract

The only deserializable input is:

```json
{
  "workspaceRoot": "C:/selected/project",
  "placeKey": "lobby"
}
```

`placeKey` may be omitted only when the validated manifest defines `project.defaultPlace`. Unknown fields are rejected, including `executable`, `sha256`, `version`, `port`, and `pid`. The wired desktop backend additionally requires `workspaceRoot` to equal the workspace selected through native trusted workspace state rather than accepting an arbitrary UI text field.

`RojoDesktopConfigAdapter` and `ResolvedRojoDesktopConfig` are deliberately not deserializable or serializable. They remain Rust-side capabilities. The adapter produces a `RojoSessionStartRequest`, which is passed directly to the Rust-side session manager; it is not returned to the webview.

## Resolution sequence

`RojoDesktopConfigAdapter::resolve` fails closed through these steps:

1. Canonicalize the selected workspace and require a directory.
2. Load and fully validate `neuman.project.yaml` through `ProjectManifest::load`.
3. Select the explicit place key or required manifest default and prove it exists.
4. Validate `repository.projectFile` as a normalized relative path, canonicalize it, require a regular file, and prove it remains inside the workspace.
5. Strictly parse the root `.project.json` and every explicit or directory-default included project. Project JSONC, unknown root fields/directives, custom sync rules, non-empty glob rules, and project-owned serve addresses fail closed because the preflight cannot prove their effective mapping statically.
6. Recursively resolve every `$path` relative to its declaring project. Paths must exist, remain inside the workspace, use normal relative components, and traverse no symlink; directory trees are bounded and reject symlinked entries.
7. Derive escaped DataModel destinations for declarative nodes, filesystem-backed subtrees, properties/attributes, and effective `$ignoreUnknownInstances` behavior. Omitting `$ignoreUnknownInstances` on a non-`$path` node is treated as Rojo's destructive `true` default.
8. Reconcile each exact/subtree claim to the selected place's ownership table. A claim must be contained by one `git-code` root, cannot cross a delegated child root, and a `$path` source must remain beneath that owner's declared `projectPath`. Mapping an ancestor of `studio-art`, `terrain`, `service-state`, `generated`, or another delegated root is rejected.
9. Reject `.rbxm`/`.rbxmx` inputs, including files discovered below a mapped directory, unless the containing `git-code` ownership entry explicitly declares `allowRojoBinaryModels: true`. The override never permits a mapping into or across a non-code root.
10. Read a regular `neuman.lock.json` no larger than 1 MiB from inside the workspace.
11. Require lock schema `1.0` and an exact `manifestHash` match with the canonical validated manifest.
12. Resolve `toolchain.rojo`, require the exact manifest and lock versions to agree, and select the current platform artifact before a top-level fallback path.
13. Resolve the relative artifact beneath the Rust-configured tool root, canonicalize it, require a regular file, and reject traversal or symlink escape.
14. Normalize the canonical `sha256:` lock value into the raw lowercase 64-hex `RojoPin` representation.
15. Call `VerifiedRojo::verify` immediately, proving the actual file checksum and exact `Rojo VERSION` response. `RojoSessionManager::start` verifies it again at process start, closing the adapter/start time-of-check window.
16. Construct the request with backend-owned restart/log policy. No PID, port, executable, checksum, or policy value comes from the webview.

The Rust-only `RojoOwnershipReport` retains the parsed project-file inventory and every effective destination/source/scope/owner decision. It is evidence for diagnostics and future preview UI; it is not accepted from the renderer. Desktop restart reruns the complete adapter preflight before the manager performs a controlled stop/start.

## Lockfile artifact shape

The adapter accepts the SPEC-03 `toolchain.rojo.version` and `sha256` fields plus a relative artifact path. Platform-specific artifacts are preferred:

```json
{
  "schemaVersion": "1.0",
  "manifestHash": "b3-256:...",
  "toolchain": {
    "rojo": {
      "version": "7.6.1",
      "sha256": "sha256:<64 lowercase hex>",
      "path": "fallback/rojo",
      "artifacts": {
        "x86_64-pc-windows-msvc": {
          "path": "x86_64-pc-windows-msvc/rojo.exe",
          "sha256": "sha256:<64 lowercase hex>"
        }
      }
    }
  }
}
```

Qualified primary keys are:

- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

For compatibility with simpler generated locks, `<os>-<arch>` and `<arch>-<os>` artifact keys are checked after the primary key. An artifact can be a relative path string when the top-level SHA is present, or a strict `{ "path", "sha256" }` object. If no current-platform artifact exists, both top-level `path` and `sha256` are required. The adapter never searches `PATH`, downloads a binary, or guesses an executable location.

Versions must be exact `MAJOR.MINOR.PATCH` strings with optional valid prerelease/build identifiers. SHA-256 accepts canonical `sha256:<digest>` from SPEC-03 or raw digest compatibility input, but the digest must always be exactly 64 lowercase hexadecimal characters.

## Tool-root policy

`RojoDesktopConfigAdapter::workspace_scoped()` uses `<workspace>/.neuman/tools`. The canonical tool root must remain inside the workspace, so a symlinked `.neuman/tools` cannot escape.

`with_trusted_tool_root` supports an application-managed external cache. That root is selected and canonicalized by Rust backend configuration, never by `RojoDesktopSelection`. In both modes the artifact itself must canonicalize beneath the tool root; traversal, missing files, directories, and escaping symlinks fail.

## Backend wiring

```rust
let selection: RojoDesktopSelection = /* typed command input */;
let resolved = adapter.resolve(&selection)?;
let outcome = session_manager.start(resolved.session_request())?;
// Return only outcome.status to the webview.
```

The desktop implements this wiring with one long-lived adapter and one mutex-protected `RojoSessionManager` in native state. The renderer receives only typed start/list/status/restart/stop results and aggregate health; the manager retains child handles, pinned capability material, and bounded logs.

## Verification

Eleven focused test groups cover:

- default and explicit place selection;
- current-platform artifact preference over an invalid fallback;
- manifest-hash binding and exact manifest/lock version agreement;
- traversal, missing default, unknown place, malformed version/SHA, and checksum mismatch rejection;
- webview rejection of executable/PID fields;
- workspace-scoped and fixed tool roots with a compiled checksum-pinned fake Rojo executable.
- nested project inclusion and nested filesystem mappings;
- path traversal and source/`projectPath` mismatch;
- unknown/dynamic project fields;
- explicit and implicit ancestor takeover of Studio-art roots;
- ambiguous binary models with required explicit owner override;
- source-tree symlink rejection when the platform permits symlink fixture creation.

The Unix test build additionally exercises an artifact symlink escaping the tool root. The adapter tests pass against the full NeuMan library, and both default and desktop-feature Clippy gates pass with warnings denied.
