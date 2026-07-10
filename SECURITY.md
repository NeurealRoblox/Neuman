# Security policy

## Supported versions

NeuMan is pre-1.0. Security fixes are made on the default branch and included in the next signed release. No development checkout or unsigned build is an official distribution.

## Reporting a vulnerability

Use GitHub private vulnerability reporting in the canonical `NeurealRoblox/Neuman` repository. Do not open a public issue for credential exposure, authentication bypass, cross-project disclosure, unsafe Git/process execution, signature/update bypass, place-publication bypass, or destructive Studio behavior.

Include the affected commit/version, platform, prerequisite access, minimal reproduction, impact, and any evidence that secrets or Roblox resources were accessed. Remove live tokens, API keys, cookies, private keys, personal data, universe IDs, and unpublished assets from the report unless maintainers provide a protected transfer method.

Maintainers should acknowledge a complete report within five business days, establish a private remediation plan, add a regression test, rotate exposed credentials, and coordinate disclosure after signed fixed builds are available. No bounty is promised.

## Security boundaries

- NeuMan never accepts `.ROBLOSECURITY` cookies or a Roblox OAuth client secret.
- Desktop OAuth tokens belong only in the supported operating-system credential vault.
- Roblox publication/Open Cloud execution keys belong only in the operator-owned automation environment.
- The renderer and Studio plugin receive neither provider nor Hub administrative credentials.
- Official updates must pass the signed-tag, native-signature, updater-signature, checksum, and provenance contract in `/docs/guides/OFFICIAL_RELEASES.md`.
