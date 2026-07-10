# NeuMan governance

NeuMan uses a maintainer-led, specification-first model during the pre-1.0 period.

## Decisions

Routine fixes are accepted through reviewed pull requests. Changes to authority, security, protocols, schemas, storage, compatibility, licensing, release provenance, or Roblox provider behavior require an ADR or a normative specification update. The change must state alternatives, migration/rollback behavior, threat impact, and qualification evidence.

Maintainers seek technical consensus. If consensus cannot be reached, the maintainers responsible for the affected subsystem record the decision and dissent in the ADR. No single provider, sponsor, storage product, or hosted service may silently become a mandatory project authority.

## Maintainers and releases

Maintainers are added or removed by a documented pull request based on sustained review-quality contributions and security judgment. Protected GitHub environments and CODEOWNERS enforce sensitive review in the canonical repository.

Only the protected official-release workflow may publish an official build. Release managers cannot waive required evidence inside a workflow run. Compromised credentials, unverifiable artifacts, provider ambiguity, or incomplete rollback evidence stop a release.

## Compatibility and deprecation

Public formats follow `/docs/specs/SPEC_00_INDEX_AND_CONVENTIONS.md`. Breaking changes require a migration path, compatibility window, fixtures, and release note. Experimental features must be labeled and cannot become an implicit authority source.

## Forks and self-hosting

Forks may use different public OAuth client IDs, signing identities, update endpoints, and self-hosted Hub deployments. They must not present themselves as official NeuMan builds or reuse official update/signing trust without authorization.
