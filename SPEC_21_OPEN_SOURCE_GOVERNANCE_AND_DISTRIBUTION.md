# SPEC-21 — Open-Source Governance and Distribution

Status: Draft  
Version: 0.1.0  
Last updated: 2026-07-09  
Depends on: SPEC-00, SPEC-18–20

## 1. Purpose

This specification defines licensing, repository structure, governance, decision making, contributions, security disclosure, releases, trademarks, documentation, community conduct, local/self-host operation, protocol openness, and project sustainability.

## 2. License

Recommended: Apache License 2.0 for NeuMan core, daemon, CLI, desktop, Hub, protocols, schemas, and Studio plugin, subject to legal review. It permits commercial/open use and includes explicit patent terms.

Third-party components retain their licenses. Rojo is orchestrated/compatible; any MPL-licensed modifications/files comply with MPL. Dependency license inventory/SBOM published per release.

No contributor content is accepted without right to license.

## 3. Contribution certification

Use Developer Certificate of Origin (DCO), not mandatory copyright assignment. Commits include Signed-off-by. Bot/generated commits have traceable authorized identity.

Large corporate contributions may receive contributor guidance but no private feature entitlement.

## 4. Repository structure

Recommended monorepo:

```text
/crates                 Rust core/daemon/CLI/Hub
/apps/desktop           Tauri/React UI
/plugins/studio         Luau plugin
/runner                 fixed Studio runner
/protocols              schemas/conformance fixtures
/specs or SPEC_*        normative specifications
/docs                   user/operator/developer docs
/fixtures               synthetic repos/native corpus manifests
/tests                  integration/E2E/security/performance
/packaging              installers/images/update metadata
/adr                    architecture decisions
```

If workspace cannot create directories initially, root spec filenames remain normative until repository layout migration is committed.

## 5. Governance roles

- Contributors
- Reviewers by area
- Maintainers
- Release managers
- Security team
- Technical Steering Committee (TSC) after community scale

Role criteria/term/removal/inactivity/conflict process are public. Employer/company does not automatically grant role.

## 6. Decision making

- Routine changes: maintainer review/consensus.
- Material architecture/protocol/schema/security: issue + ADR/spec PR + area owners.
- Breaking change: migration/compat/security analysis and public comment window.
- Governance/license/trademark: TSC/owner vote per charter.
- Urgent security fix may use private process, followed by public record when safe.

Consensus preferred. If unresolved, documented vote with conflicts disclosed.

## 7. Code ownership

CODEOWNERS areas:

- domain/schema/protocol;
- security/auth/credentials/update;
- Studio plugin/native serialization;
- build/release/publish;
- Hub/storage;
- desktop UX/accessibility;
- packaging/release;
- specs/governance.

Security-critical changes require at least two qualified reviewers or explicit small-project exception documented.

## 8. Contribution workflow

1. Issue/discussion for material work.
2. Fork/branch and DCO commits.
3. Tests/spec/docs with change.
4. CI and review.
5. No generated binary without source/repro recipe.
6. Squash/rebase/merge policy documented; history retains authorship.
7. Changelog label and compatibility impact.

Good-first issues avoid security/production-critical changes unless mentored.

## 9. Specification-first rule

Behavioral changes to public/persisted/security/release contracts update relevant spec and SPEC-22 before/with implementation. PR template asks:

- requirements affected;
- schema/protocol change;
- migration;
- security/privacy;
- rollback;
- compatibility/tests/docs.

## 10. Release governance

- Release manager separate from sole code author where practical.
- Protected tags/workflows/signing.
- Public release checklist and provenance.
- SemVer/channels from SPEC-20.
- Security embargo process.
- No unreviewed direct binary upload.
- Reproducibility verification by second environment for stable release.

## 11. Security policy

Public `SECURITY.md` includes supported versions, private contact, expected response, safe harbor, scope, encryption key, and coordinated disclosure. Security reports never go to public issue first.

Security team has key/credential revocation and emergency release authority under audited process.

## 12. Code of conduct

Adopt Contributor Covenant or equivalent. Publish enforcement contacts/process, confidentiality, appeals, conflict handling. Roblox creator community accessibility and minors/safety considerations inform moderation.

## 13. Documentation

Required sets:

- concepts/source-of-truth;
- setup local/self-hosted;
- Roblox/GitHub app registration;
- project manifest/ownership;
- artist/developer/reviewer/release workflows;
- security/privacy/threat model;
- operations/backups/upgrades;
- protocol/schema/API references;
- migration/troubleshooting;
- compatibility/known limitations;
- contributor architecture/testing.

Docs versioned with release; examples tested. No unofficial API workaround presented as supported.

## 14. Protocol and schema openness

- Public specifications and canonical schemas.
- Independent conformance suite.
- No proprietary encryption/compression required for core interoperability.
- Provider/Hub export format open.
- Extensions cannot be required to read one's own accepted/release data.
- Compatibility and deprecation public.

## 15. No official hosted control plane

The official NeuMan project MUST NOT operate a user account system, Roblox OAuth proxy, multi-tenant Hub, central project/art database, or default telemetry collector. Official desktop operation is local-first, and team features connect only to endpoints deliberately configured by the user or organization.

The complete correctness-critical Hub/lock/revision/release implementation, public protocol, storage provider interfaces, export/import, migration, backup, and conformance tests remain open source. Core security fixes are never paywalled.

A third party MAY offer compatible managed operations/support/scale under its own identity and terms. It MUST NOT call its service the official NeuMan control plane, receive official signing keys, or make a fork build appear official. Trademark policy permits truthful compatibility statements. Users can export data and move to a local or self-hosted deployment without a proprietary format.

## 16. Trademarks

Project name/logo trademark policy distinguishes official releases/services from forks while permitting truthful compatibility statements. Roblox/Epic/GitHub marks used only under their policies; project MUST NOT imply endorsement.

## 17. Plugin distribution

- Creator Store listing links source, privacy, terms, docs, version.
- Local/plugin release checksum and source tag.
- No hidden network destinations/telemetry.
- Update compatibility with desktop clearly stated.
- Stable plugin remains usable with self-hosted/local mode.

## 18. Dependency/upstream policy

- Prefer upstream contribution over permanent private forks, especially Rojo/rbx-dom.
- Forks document reason, diff, update owner, exit plan, license.
- Dependabot/Renovate-like updates require tests; no blind auto-merge for privileged dependencies.
- Vulnerability response/SBOM.
- Vendor only when reproducibility/security requires and license permits.

## 19. Roadmap and issues

Public roadmap separates committed/exploring. Issues labeled component, impact, difficulty, compatibility, security. No promise dates without capacity. Design discussions retained.

## 20. Telemetry ethics

- No project-operated collection endpoint or network telemetry in the default build.
- Any future telemetry is separately opt-in, uses a documented schema, supports a user-selected/self-hosted endpoint and a complete off switch, and requires a privacy/spec review.
- No source/art/Roblox user profiling.
- Aggregate product reliability only.
- Community review of telemetry changes.
- Raw telemetry not sold or used for model training.

## 21. Sustainability

Potential funding: sponsorships, support/consulting, grants, and independent compatible services. Funding disclosures and sponsor influence policy. No contributor feature priority secretly sold against public governance, and funding never changes the no-official-control-plane invariant without a new project and explicit user migration.

Bus-factor controls: multiple maintainers, documented release/keys, recovery contacts, automated reproducible process.

## 22. Archival/fork contingency

If project winds down:

- announce and freeze stable release;
- publish keys/revocation/verification guidance as safe;
- ensure export tools/docs remain;
- transfer governance/trademark under transparent process or archive;
- because the project operates no hosted user data, archival has no central customer-data migration; third-party operators remain responsible for their own export/retention promises.

## 23. Acceptance criteria

1. Legal review approves license/trademark/privacy/Roblox terms posture.
2. DCO, governance, code of conduct, security, contribution, release docs exist.
3. Public conformance/export prevents third-party service lock-in and no official workflow requires a NeuMan-operated runtime.
4. Every release has source tag, checksums, signature, SBOM, provenance, specs, changelog.
5. Upstream/fork license obligations tested/reviewed.
6. Creator Store plugin maps to public source/version/privacy terms.
