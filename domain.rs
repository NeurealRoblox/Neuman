//! Canonical, provider-neutral domain types for NeuMan.
//!
//! Durable identifiers are strings at every serialization boundary. Hashing uses
//! canonical JSON with explicit domain separators so independent components can
//! reproduce identities without sharing a database.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::{Uuid, Version};

/// A validation failure in a durable domain value.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// A prefixed identifier was malformed.
    #[error("invalid {kind}: {reason}")]
    InvalidId {
        /// Identifier kind.
        kind: &'static str,
        /// Stable, user-safe reason.
        reason: String,
    },
    /// A hash was malformed.
    #[error("invalid content hash: {0}")]
    InvalidHash(String),
    /// Canonical serialization failed.
    #[error("canonical serialization failed: {0}")]
    CanonicalJson(String),
    /// A state transition is not legal.
    #[error("illegal {aggregate} transition from {from} to {to}")]
    IllegalTransition {
        /// Aggregate name.
        aggregate: &'static str,
        /// Current state.
        from: String,
        /// Requested state.
        to: String,
    },
}

macro_rules! uuid_v7_id {
    ($name:ident, $prefix:literal, $label:literal) => {
        #[doc = concat!("Canonical ", $label, " identifier backed by UUIDv7.")]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a time-ordered UUIDv7 identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Returns the UUID payload.
            #[must_use]
            pub const fn uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0.hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let Some(raw) = value.strip_prefix($prefix) else {
                    return Err(DomainError::InvalidId {
                        kind: $label,
                        reason: concat!("expected prefix ", $prefix).into(),
                    });
                };
                let id = Uuid::parse_str(raw).map_err(|_| DomainError::InvalidId {
                    kind: $label,
                    reason: "expected lowercase canonical UUID".into(),
                })?;
                if id.get_version() != Some(Version::SortRand)
                    || value != format!(concat!($prefix, "{}"), id.hyphenated())
                {
                    return Err(DomainError::InvalidId {
                        kind: $label,
                        reason: "expected lowercase canonical UUIDv7".into(),
                    });
                }
                Ok(Self(id))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                raw.parse().map_err(de::Error::custom)
            }
        }
    };
}

uuid_v7_id!(ProjectId, "prj_", "project ID");
uuid_v7_id!(WorkspaceId, "wsp_", "workspace ID");
uuid_v7_id!(SessionId, "ses_", "session ID");
uuid_v7_id!(ArtRevisionId, "art_", "art revision ID");
uuid_v7_id!(BuildId, "bld_", "build ID");
uuid_v7_id!(ReleaseId, "rel_", "release ID");
uuid_v7_id!(OperationId, "op_", "operation ID");
uuid_v7_id!(LockId, "lck_", "lock ID");

/// Stable art-cell identifier. Cell IDs are UUIDv4 because they are created in Studio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId(Uuid);

impl CellId {
    /// Creates a random cell identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for CellId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CellId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cell_{}", self.0.hyphenated())
    }
}

impl FromStr for CellId {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(raw) = value.strip_prefix("cell_") else {
            return Err(DomainError::InvalidId {
                kind: "cell ID",
                reason: "expected prefix cell_".into(),
            });
        };
        let id = Uuid::parse_str(raw).map_err(|_| DomainError::InvalidId {
            kind: "cell ID",
            reason: "expected UUID".into(),
        })?;
        if id.get_version() != Some(Version::Random) || value != format!("cell_{}", id.hyphenated())
        {
            return Err(DomainError::InvalidId {
                kind: "cell ID",
                reason: "expected lowercase canonical UUIDv4".into(),
            });
        }
        Ok(Self(id))
    }
}

impl Serialize for CellId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CellId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// BLAKE3-256 content identity encoded as lowercase RFC 4648 base32 without padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hashes bytes with BLAKE3-256.
    #[must_use]
    pub fn digest(bytes: impl AsRef<[u8]>) -> Self {
        Self(*blake3::hash(bytes.as_ref()).as_bytes())
    }

    /// Returns the raw 32-byte digest.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b3-256:{}", encode_base32(&self.0))
    }
}

impl FromStr for ContentHash {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let raw = value
            .strip_prefix("b3-256:")
            .ok_or_else(|| DomainError::InvalidHash("expected b3-256 prefix".into()))?;
        let bytes = decode_base32(raw)?;
        if bytes.len() != 32 {
            return Err(DomainError::InvalidHash(
                "BLAKE3-256 digest must be 32 bytes".into(),
            ));
        }
        let mut result = [0; 32];
        result.copy_from_slice(&bytes);
        if value != Self(result).to_string() {
            return Err(DomainError::InvalidHash(
                "hash is not canonical lowercase base32".into(),
            ));
        }
        Ok(Self(result))
    }
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// SHA-256 identity used when an external format or lockfile requires SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Hash([u8; 32]);

impl Sha256Hash {
    /// Hashes exact bytes with SHA-256.
    #[must_use]
    pub fn digest(bytes: impl AsRef<[u8]>) -> Self {
        Self(Sha256::digest(bytes.as_ref()).into())
    }

    /// Returns the raw 32-byte digest.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for Sha256Hash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sha256:{}", hex::encode(self.0))
    }
}

impl FromStr for Sha256Hash {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let raw = value
            .strip_prefix("sha256:")
            .ok_or_else(|| DomainError::InvalidHash("expected sha256 prefix".into()))?;
        if raw.len() != 64
            || !raw
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(DomainError::InvalidHash(
                "SHA-256 must be 64 lowercase hexadecimal characters".into(),
            ));
        }
        let decoded = hex::decode(raw)
            .map_err(|_| DomainError::InvalidHash("invalid SHA-256 hexadecimal digest".into()))?;
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(&decoded);
        Ok(Self(digest))
    }
}

impl Serialize for Sha256Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

fn encode_base32(bytes: &[u8]) -> String {
    let mut result = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            result.push(char::from(BASE32[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits != 0 {
        result.push(char::from(BASE32[((buffer << (5 - bits)) & 31) as usize]));
    }
    result
}

fn decode_base32(value: &str) -> Result<Vec<u8>, DomainError> {
    let mut result = Vec::with_capacity(value.len() * 5 / 8);
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for ch in value.bytes() {
        let part = match ch {
            b'a'..=b'z' => ch - b'a',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => {
                return Err(DomainError::InvalidHash(
                    "base32 must be lowercase and unpadded".into(),
                ));
            }
        };
        buffer = (buffer << 5) | u32::from(part);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            result.push(((buffer >> bits) & 255) as u8);
        }
    }
    if bits != 0 && (buffer & ((1_u32 << bits) - 1)) != 0 {
        return Err(DomainError::InvalidHash(
            "non-zero base32 padding bits".into(),
        ));
    }
    Ok(result)
}

/// Roblox integer identity preserved as decimal text at JSON boundaries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RobloxId(String);

impl RobloxId {
    /// Returns the exact decimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RobloxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for RobloxId {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(DomainError::InvalidId {
                kind: "Roblox ID",
                reason: "expected canonical unsigned decimal string".into(),
            });
        }
        Ok(Self(value.into()))
    }
}

impl<'de> Deserialize<'de> for RobloxId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// Repository object format.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GitObjectFormat {
    /// Forty-hex Git object IDs.
    Sha1,
    /// Sixty-four-hex Git object IDs.
    Sha256,
}

/// Validated Git object identifier.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct GitOid(String);

impl GitOid {
    /// Validates an OID against the repository object format.
    pub fn parse_for(value: &str, format: GitObjectFormat) -> Result<Self, DomainError> {
        let expected = match format {
            GitObjectFormat::Sha1 => 40,
            GitObjectFormat::Sha256 => 64,
        };
        if value.len() != expected
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(DomainError::InvalidId {
                kind: "Git OID",
                reason: format!("expected {expected} lowercase hexadecimal characters"),
            });
        }
        Ok(Self(value.into()))
    }

    /// Returns the hexadecimal object ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The immutable metadata for one native Studio cell snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CellSnapshot {
    /// Hash of exact RBXM bytes.
    pub content_hash: ContentHash,
    /// Byte length of the RBXM object.
    pub size_bytes: u64,
    /// Must be `application/x-roblox-rbxm`.
    pub media_type: String,
    /// NeuMan serialization profile.
    pub serialization_version: String,
    /// Studio build that produced the bytes.
    pub studio_build: String,
    /// Versioned Roblox API schema.
    pub api_schema_hash: ContentHash,
    /// RFC 3339 UTC capture time.
    pub captured_at: String,
    /// Stable principal identifier or local actor label.
    pub captured_by: String,
    /// Canonical semantic index object.
    pub semantic_index_hash: ContentHash,
}

/// The complete state entry for a cell in an art revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtCellState {
    /// Stable cell identity.
    pub cell_id: CellId,
    /// Exact native RBXM content identity.
    pub snapshot_hash: ContentHash,
    /// Declared DataModel slot.
    pub slot_path: String,
}

/// An immutable art revision. Accepted records are never rewritten.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtRevision {
    /// Revision identity.
    pub art_revision_id: ArtRevisionId,
    /// Project identity.
    pub project_id: ProjectId,
    /// Manifest channel key.
    pub channel_id: String,
    /// Zero, one, or two parent revisions.
    pub parents: Vec<ArtRevisionId>,
    /// Complete sorted art state.
    pub cells: BTreeMap<CellId, ArtCellState>,
    /// Merkle-like identity of the complete state.
    pub state_root_hash: ContentHash,
    /// Author identity.
    pub author: String,
    /// Human revision message.
    pub message: String,
    /// RFC 3339 UTC creation time.
    pub created_at: String,
    /// Lifecycle status.
    pub status: ArtRevisionStatus,
}

/// Art review lifecycle.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtRevisionStatus {
    /// Locally captured draft.
    Capturing,
    /// Submitted for validation/review.
    Proposed,
    /// Automated validation in progress.
    Validating,
    /// Waiting for human review.
    ReviewRequired,
    /// Immutable accepted revision.
    Accepted,
    /// Rejected proposal.
    Rejected,
    /// Capture or validation failed.
    Failed,
    /// Accepted but no longer channel head.
    Superseded,
}

impl ArtRevision {
    /// Computes the version-1 state root from complete, sorted cell state.
    pub fn compute_state_root(
        cells: &BTreeMap<CellId, ArtCellState>,
    ) -> Result<ContentHash, DomainError> {
        if cells.is_empty() {
            return Ok(ContentHash::digest(b"neuman-art-empty-v1\0"));
        }
        let mut level = Vec::with_capacity(cells.len());
        for (cell_id, cell) in cells {
            if *cell_id != cell.cell_id {
                return Err(DomainError::InvalidId {
                    kind: "art cell state",
                    reason: "map key and embedded CellId differ".into(),
                });
            }
            let key = format!("cell/{cell_id}");
            let value = cell.snapshot_hash.to_string();
            let key_len = u32::try_from(key.len()).map_err(|_| DomainError::InvalidId {
                kind: "art resource key",
                reason: "key exceeds U32 length".into(),
            })?;
            let value_len = u32::try_from(value.len()).map_err(|_| DomainError::InvalidId {
                kind: "art resource hash",
                reason: "hash exceeds U32 length".into(),
            })?;
            let mut leaf = blake3::Hasher::new();
            leaf.update(b"neuman-art-leaf-v1\0");
            leaf.update(&key_len.to_be_bytes());
            leaf.update(key.as_bytes());
            leaf.update(&value_len.to_be_bytes());
            leaf.update(value.as_bytes());
            level.push(*leaf.finalize().as_bytes());
        }
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            for pair in level.chunks(2) {
                if let [left, right] = pair {
                    let mut node = blake3::Hasher::new();
                    node.update(b"neuman-art-node-v1\0");
                    node.update(left);
                    node.update(right);
                    next.push(*node.finalize().as_bytes());
                } else {
                    next.push(pair[0]);
                }
            }
            level = next;
        }
        Ok(ContentHash(level[0]))
    }

    /// Validates structural and derived invariants.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.parents.len() > 2 {
            return Err(DomainError::InvalidId {
                kind: "art parents",
                reason: "at most two parents are allowed".into(),
            });
        }
        if self.channel_id.is_empty() || self.channel_id.len() > 64 {
            return Err(DomainError::InvalidId {
                kind: "art channel",
                reason: "must be 1..64 characters".into(),
            });
        }
        if Self::compute_state_root(&self.cells)? != self.state_root_hash {
            return Err(DomainError::InvalidHash(
                "art state root does not match cells".into(),
            ));
        }
        Ok(())
    }
}

/// Canonical logical build input. Actor, request time, and message are excluded.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildRepositoryIdentity {
    /// Stable provider repository identity.
    pub id: String,
    /// Git object format used to interpret `codeCommit`.
    pub object_format: GitObjectFormat,
}

/// Canonical logical build input. Actor, request time, and message are excluded.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogicalBuildInput {
    /// Format version.
    pub schema_version: String,
    /// Project identity.
    pub project_id: ProjectId,
    /// Manifest place key.
    pub place_key: String,
    /// Stable repository identity and object format.
    pub repository: BuildRepositoryIdentity,
    /// Exact code commit.
    pub code_commit: GitOid,
    /// Accepted art revision.
    pub art_revision_id: ArtRevisionId,
    /// Complete art state identity.
    pub art_state_root_hash: ContentHash,
    /// Base place/template object.
    pub base_template_hash: ContentHash,
    /// Dependency manifest object.
    pub dependency_manifest_hash: ContentHash,
    /// Exact toolchain lock object.
    pub toolchain_lock_hash: ContentHash,
    /// Snapshot of effective policy.
    pub policy_revision_hash: ContentHash,
    /// Manifest at the code commit.
    pub manifest_hash: ContentHash,
    /// Build profile, e.g. `release`.
    pub profile: String,
}

impl LogicalBuildInput {
    /// Computes `LogicalBuildHash v1`.
    pub fn logical_hash(&self) -> Result<ContentHash, DomainError> {
        hash_canonical("neuman-logical-build-v1\0", self)
    }
}

/// One immutable artifact in a release bundle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleArtifact {
    /// Artifact role, e.g. `place-candidate`.
    pub name: String,
    /// CAS object identity.
    pub content_hash: ContentHash,
    /// Exact size.
    pub size_bytes: u64,
    /// Registered media type.
    pub media_type: String,
}

/// Environment-neutral immutable release bundle manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseBundleManifest {
    /// Format version.
    pub schema_version: String,
    /// Logical input identity.
    pub logical_build_hash: ContentHash,
    /// Place binding, but never a production target or credential.
    pub place_key: String,
    /// Ordered artifacts.
    pub artifacts: Vec<BundleArtifact>,
    /// Reproducibility claim supported by evidence.
    pub reproducibility: Reproducibility,
}

impl ReleaseBundleManifest {
    /// Computes the immutable bundle identity.
    pub fn bundle_hash(&self) -> Result<ContentHash, DomainError> {
        hash_canonical("neuman-release-bundle-v1\0", self)
    }
}

/// Strength of reproducibility evidence.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Reproducibility {
    /// Canonical inputs reproduce the same logical build identity.
    Input,
    /// Managed DataModel state is semantically equivalent.
    Semantic,
    /// Final artifact bytes are identical.
    ByteExact,
}

/// Release lifecycle persisted by the local ledger and Hub.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseStatus {
    /// Created but not approved.
    Draft,
    /// Waiting for approvals.
    AwaitingApproval,
    /// Required approvals are present.
    Approved,
    /// Per-target preflight is running.
    Preflighting,
    /// External mutation may be in progress.
    Publishing,
    /// All targets passed required post-publish checks.
    Published,
    /// Some, but not all, targets crossed a commit point.
    PartiallyPublished,
    /// Policy requires compensation.
    RollbackRequired,
    /// No target changed.
    FailedNoChange,
    /// Provider state could not be reconciled.
    UnknownExternalState,
    /// Compensation is active.
    RollingBack,
    /// Compensation completed.
    RolledBack,
    /// Compensation failed.
    RollbackFailed,
}

impl ReleaseStatus {
    /// Checks the durable release state machine.
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        use ReleaseStatus as S;
        let allowed = matches!(
            (self, next),
            (S::Draft, S::AwaitingApproval | S::Approved)
                | (S::AwaitingApproval, S::Approved)
                | (S::Approved, S::Preflighting)
                | (
                    S::Preflighting | S::PartiallyPublished | S::UnknownExternalState,
                    S::Publishing
                )
                | (S::Preflighting | S::Publishing, S::FailedNoChange)
                | (
                    S::Publishing,
                    S::Published | S::PartiallyPublished | S::UnknownExternalState
                )
                | (S::PartiallyPublished, S::RollbackRequired)
                | (S::RollbackRequired | S::Published, S::RollingBack)
                | (S::RollingBack, S::RolledBack | S::RollbackFailed)
        );
        if !allowed {
            return Err(DomainError::IllegalTransition {
                aggregate: "release",
                from: format!("{self:?}"),
                to: format!("{next:?}"),
            });
        }
        Ok(next)
    }
}

/// Produces RFC 8785 JSON Canonicalization Scheme bytes.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    serde_jcs::to_vec(value).map_err(|error| DomainError::CanonicalJson(error.to_string()))
}

/// Hashes canonical JSON using a stable domain prefix.
pub fn hash_canonical<T: Serialize>(domain: &str, value: &T) -> Result<ContentHash, DomainError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&canonical_json(value)?);
    Ok(ContentHash(*hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_versioned_and_json_strings() {
        let id = ProjectId::new();
        let text = id.to_string();
        assert!(text.starts_with("prj_"));
        assert_eq!(text.parse::<ProjectId>().unwrap(), id);
        assert_eq!(
            serde_json::from_str::<ProjectId>(&serde_json::to_string(&id).unwrap()).unwrap(),
            id
        );
        assert!(
            "prj_00000000-0000-4000-8000-000000000000"
                .parse::<ProjectId>()
                .is_err()
        );
    }

    #[test]
    fn content_hash_round_trips_and_is_canonical() {
        let hash = ContentHash::digest(b"NeuMan");
        assert_eq!(hash.to_string().len(), "b3-256:".len() + 52);
        assert_eq!(hash.to_string().parse::<ContentHash>().unwrap(), hash);
        assert!(
            hash.to_string()
                .to_uppercase()
                .parse::<ContentHash>()
                .is_err()
        );
    }

    #[test]
    fn sha256_hash_is_prefixed_lowercase_hex() {
        let hash = Sha256Hash::digest(b"NeuMan");
        assert_eq!(hash.to_string().len(), "sha256:".len() + 64);
        assert_eq!(hash.to_string().parse::<Sha256Hash>().unwrap(), hash);
        assert!(
            hash.to_string()
                .to_uppercase()
                .parse::<Sha256Hash>()
                .is_err()
        );
    }

    #[test]
    fn canonical_json_sorts_nested_keys() {
        let value = serde_json::json!({"z": {"b": 1, "a": 2}, "a": 0});
        assert_eq!(
            String::from_utf8(canonical_json(&value).unwrap()).unwrap(),
            r#"{"a":0,"z":{"a":2,"b":1}}"#
        );
    }

    #[test]
    fn canonical_json_uses_ecmascript_numbers_and_escapes() {
        let numbers = serde_json::json!({"round": 333_333_333.333_333_3_f64, "negative_zero": -0.0_f64, "exp": 1e30_f64});
        assert_eq!(
            String::from_utf8(canonical_json(&numbers).unwrap()).unwrap(),
            r#"{"exp":1e+30,"negative_zero":0,"round":333333333.3333333}"#
        );
        let string = serde_json::json!({"s": "\u{000f}\n\"\\€"});
        assert_eq!(
            String::from_utf8(canonical_json(&string).unwrap()).unwrap(),
            r#"{"s":"\u000f\n\"\\€"}"#
        );
    }

    #[test]
    fn canonical_json_sorts_properties_by_utf16_code_units() {
        let value = serde_json::json!({"\u{e000}": 2, "😀": 1});
        assert_eq!(
            String::from_utf8(canonical_json(&value).unwrap()).unwrap(),
            "{\"😀\":1,\"\":2}"
        );
    }

    #[test]
    fn art_state_root_uses_resource_identity_and_snapshot_hash() {
        let cell = CellId::new();
        let mut state = BTreeMap::new();
        state.insert(
            cell,
            ArtCellState {
                cell_id: cell,
                snapshot_hash: ContentHash::digest(b"one"),
                slot_path: "/Workspace/Art/A".into(),
            },
        );
        let first = ArtRevision::compute_state_root(&state).unwrap();
        state.get_mut(&cell).unwrap().snapshot_hash = ContentHash::digest(b"two");
        assert_ne!(first, ArtRevision::compute_state_root(&state).unwrap());
    }

    #[test]
    fn art_state_empty_root_matches_spec() {
        let state = BTreeMap::new();
        assert_eq!(
            ArtRevision::compute_state_root(&state).unwrap(),
            ContentHash::digest(b"neuman-art-empty-v1\0")
        );
    }

    #[test]
    fn art_state_single_leaf_golden_vector() {
        let cell: CellId = "cell_00000000-0000-4000-8000-000000000000".parse().unwrap();
        let mut state = BTreeMap::new();
        state.insert(
            cell,
            ArtCellState {
                cell_id: cell,
                snapshot_hash: ContentHash::digest(b"snapshot"),
                slot_path: "/Workspace/Art/Golden".into(),
            },
        );
        assert_eq!(
            ArtRevision::compute_state_root(&state).unwrap().to_string(),
            "b3-256:axh73sceatr7uhsxnke6ssnoreuppxqwbrg7gzgf5qcw4rtnyswq"
        );
    }

    #[test]
    fn build_identity_excludes_nothing_implicit() {
        let input = LogicalBuildInput {
            schema_version: "1.0".into(),
            project_id: ProjectId::new(),
            place_key: "lobby".into(),
            repository: BuildRepositoryIdentity {
                id: "github:123".into(),
                object_format: GitObjectFormat::Sha1,
            },
            code_commit: GitOid::parse_for(&"a".repeat(40), GitObjectFormat::Sha1).unwrap(),
            art_revision_id: ArtRevisionId::new(),
            art_state_root_hash: ContentHash::digest(b"art"),
            base_template_hash: ContentHash::digest(b"base"),
            dependency_manifest_hash: ContentHash::digest(b"deps"),
            toolchain_lock_hash: ContentHash::digest(b"tools"),
            policy_revision_hash: ContentHash::digest(b"policy"),
            manifest_hash: ContentHash::digest(b"manifest"),
            profile: "release".into(),
        };
        assert_eq!(input.logical_hash().unwrap(), input.logical_hash().unwrap());
        let mut changed = input.clone();
        changed.place_key = "arena".into();
        assert_ne!(
            input.logical_hash().unwrap(),
            changed.logical_hash().unwrap()
        );
    }

    #[test]
    fn release_state_machine_rejects_skip() {
        assert!(
            ReleaseStatus::Draft
                .transition(ReleaseStatus::Published)
                .is_err()
        );
        assert_eq!(
            ReleaseStatus::Approved
                .transition(ReleaseStatus::Preflighting)
                .unwrap(),
            ReleaseStatus::Preflighting
        );
    }
}
