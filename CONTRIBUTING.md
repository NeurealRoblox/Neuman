# Contributing to NeuMan

NeuMan accepts focused issues and pull requests that preserve the authority and security invariants in `SPEC_01_PRODUCT_BOUNDARIES_AND_INVARIANTS.md`.

## Development workflow

1. Discuss protocol, schema, authority, credential, or compatibility changes in an issue or ADR before implementation.
2. Keep provider credentials out of repositories, fixtures, command lines, renderer messages, Studio messages, logs, and screenshots.
3. Add deterministic tests for success, denial, replay, stale state, crash recovery, and hostile input as applicable.
4. Run the repository gates:

   ```text
   cargo fmt --all -- --check
   cargo clippy --locked --all-targets -- -D warnings
   cargo test --locked --all-targets
   cargo clippy --locked --features desktop --bin neuman-desktop -- -D warnings
   cargo test --locked --features desktop --bin neuman-desktop
   npm ci
   npm run check:studio
   npm run build
   node release_contract_test.mjs
   ```

5. Update the relevant specification, traceability row, compatibility note, and `IMPLEMENTATION_STATUS.md`. Never turn an unqualified provider or Studio behavior into an implemented claim.
6. Sign off commits with `git commit -s`. The sign-off certifies the Developer Certificate of Origin 1.1: you have the right to submit the work under this repository's license.

## Review policy

Security boundaries, wire protocols, schemas, migrations, release workflows, authority rules, cryptography, and provider adapters require maintainer review. Generated artifacts and dependency-lock changes must be reproducible from the reviewed source change. Do not weaken a fail-closed gate merely to make a demo pass.

Use GitHub's private vulnerability reporting for suspected security issues; follow `SECURITY.md` rather than opening a public exploit report.
