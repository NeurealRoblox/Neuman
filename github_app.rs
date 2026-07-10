//! Security-sensitive GitHub App primitives for `NeuMan`'s control plane.
//!
//! This module deliberately excludes PAT and broad OAuth flows.  Its public API
//! makes repository numeric identity, installation identity, secret lifetimes,
//! webhook authentication, and replay handling explicit.  Webhook JSON cannot
//! be obtained until the raw body has passed HMAC verification.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    num::NonZeroU64,
    str::FromStr,
    sync::Mutex,
};
use subtle::ConstantTimeEq as _;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

const PUBLIC_API_ORIGIN: &str = "https://api.github.com/";
const API_VERSION: &str = "2022-11-28";
const DEFAULT_USER_AGENT: &str = "NeuMan-GitHub-App/0.1";
const MAX_WEBHOOK_BODY_BYTES: usize = 5 * 1024 * 1024;
const MARKER_PREFIX: &str = "<!-- neuman:v1:";
const MARKER_SUFFIX: &str = " -->";

/// Errors returned by GitHub App integration primitives.
#[derive(Debug, thiserror::Error)]
pub enum GithubAppError {
    /// A typed provider identifier was zero or otherwise invalid.
    #[error("invalid {kind} identifier")]
    InvalidIdentifier {
        /// Identifier category used in the diagnostic.
        kind: &'static str,
    },
    /// A secret did not meet the minimum entropy/length policy.
    #[error("{kind} secret does not meet minimum length")]
    WeakSecret {
        /// Secret category used in the diagnostic.
        kind: &'static str,
    },
    /// The webhook request exceeded its strict raw-body limit.
    #[error("webhook body is too large")]
    WebhookTooLarge,
    /// A required webhook envelope value was missing or malformed.
    #[error("invalid webhook envelope: {0}")]
    InvalidWebhookEnvelope(&'static str),
    /// The webhook event is not in the configured allowlist.
    #[error("webhook event is not allowed: {0}")]
    WebhookEventNotAllowed(String),
    /// No current or overlap-period secret authenticated the raw body.
    #[error("GHA_WEBHOOK_SIGNATURE_INVALID")]
    WebhookSignatureInvalid,
    /// One delivery identifier was observed with different content.
    #[error("GHA_WEBHOOK_REPLAY_CONFLICT for delivery {delivery_id}")]
    WebhookReplayConflict {
        /// Conflicting GitHub delivery identifier.
        delivery_id: String,
    },
    /// The durable replay store could not atomically reserve a delivery.
    #[error("webhook replay store failed: {0}")]
    ReplayStore(String),
    /// JSON was invalid after the authenticated envelope was accepted.
    #[error("invalid authenticated GitHub JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// An App private key could not sign an RS256 JWT.
    #[error("GHA_TOKEN_FAILED: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    /// An outbound URL escaped the public GitHub API allowlist.
    #[error("GitHub API URL is not allowlisted")]
    UrlNotAllowlisted,
    /// The HTTP transport failed before a provider response was received.
    #[error("GHA_API_UNAVAILABLE: {0}")]
    Transport(String),
    /// GitHub returned a response that does not satisfy the operation contract.
    #[error("GitHub API response {status}: {message}")]
    Provider {
        /// HTTP-like provider status.
        status: u16,
        /// Sanitized provider diagnostic.
        message: String,
    },
    /// The API repository numeric ID did not match the project binding.
    #[error("GHA_REPOSITORY_MISMATCH: expected {expected}, observed {observed}")]
    RepositoryMismatch {
        /// Bound repository numeric ID.
        expected: u64,
        /// Repository numeric ID observed in the other credential/response.
        observed: u64,
    },
    /// A repository display name from GitHub was malformed.
    #[error("GitHub returned an invalid repository full_name")]
    InvalidRepositoryName,
    /// A marker was absent, ambiguous, malformed, unauthenticated, or invalid.
    #[error("invalid NeuMan hidden marker: {0}")]
    InvalidMarker(&'static str),
}

/// Numeric GitHub App identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(NonZeroU64);

/// Numeric GitHub App installation identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstallationId(NonZeroU64);

/// Authoritative numeric GitHub repository identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepositoryId(NonZeroU64);

macro_rules! numeric_id {
    ($ty:ident, $kind:literal) => {
        impl $ty {
            /// Construct a validated non-zero GitHub identifier.
            ///
            /// # Errors
            ///
            /// Returns [`GithubAppError::InvalidIdentifier`] when `value` is zero.
            pub fn new(value: u64) -> Result<Self, GithubAppError> {
                NonZeroU64::new(value)
                    .map(Self)
                    .ok_or(GithubAppError::InvalidIdentifier { kind: $kind })
            }

            /// Return the provider numeric identifier.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }
    };
}

numeric_id!(AppId, "App");
numeric_id!(InstallationId, "installation");
numeric_id!(RepositoryId, "repository");

/// A project-to-installation binding. Numeric repository ID is authoritative.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryBinding {
    /// Project-scoped stable identifier.
    pub project_id: String,
    /// The App installation authorized for this project.
    pub installation_id: InstallationId,
    /// The exact repository authorized for this project.
    pub repository_id: RepositoryId,
    /// Non-authoritative owner/name cache for display only.
    pub display_full_name: Option<String>,
}

impl RepositoryBinding {
    /// Validate fields which cannot be represented by the numeric newtypes.
    ///
    /// # Errors
    ///
    /// Returns [`GithubAppError::InvalidIdentifier`] for an unsafe project ID.
    pub fn validate(&self) -> Result<(), GithubAppError> {
        if self.project_id.len() < 8
            || self.project_id.len() > 128
            || !self
                .project_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(GithubAppError::InvalidIdentifier { kind: "project" });
        }
        Ok(())
    }
}

/// Secret bytes which redact debug output and overwrite their allocation on drop.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Copy secret bytes into an owned, redacted container.
    ///
    /// # Errors
    ///
    /// Returns [`GithubAppError::WeakSecret`] for fewer than 32 bytes.
    pub fn new(bytes: impl AsRef<[u8]>, kind: &'static str) -> Result<Self, GithubAppError> {
        let bytes = bytes.as_ref();
        if bytes.len() < 32 {
            return Err(GithubAppError::WeakSecret { kind });
        }
        Ok(Self(bytes.to_vec()))
    }

    fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes([REDACTED])")
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// An opaque provider token with redacted debug output and drop-time overwrite.
pub struct SecretString(String);

impl SecretString {
    fn from_provider(value: String) -> Result<Self, GithubAppError> {
        if value.len() < 20 || !value.is_ascii() {
            return Err(GithubAppError::Provider {
                status: 502,
                message: "provider returned a malformed token".to_owned(),
            });
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        // SAFETY is forbidden in this crate. String cannot expose a stable mutable
        // byte slice safely, so replace it before release; allocator behavior is
        // not a substitute for an OS secret manager.
        self.0.clear();
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }

    let mut inner_key = [0x36_u8; BLOCK];
    let mut outer_key = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_key[index] ^= normalized[index];
        outer_key[index] ^= normalized[index];
    }
    let inner = Sha256::new()
        .chain_update(inner_key)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(outer_key)
        .chain_update(inner)
        .finalize()
        .into()
}

/// The authenticated, non-JSON GitHub webhook envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedWebhook {
    /// Provider-unique delivery identifier.
    pub delivery_id: String,
    /// Allowlisted GitHub event name.
    pub event: String,
    /// SHA-256 of the exact authenticated raw bytes.
    pub body_sha256: [u8; 32],
    raw_body: Vec<u8>,
}

impl VerifiedWebhook {
    /// Parse the body only after HMAC verification has succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`GithubAppError::InvalidJson`] if authenticated bytes do not
    /// deserialize to `T`.
    pub fn parse_json<T: DeserializeOwned>(&self) -> Result<T, GithubAppError> {
        Ok(serde_json::from_slice(&self.raw_body)?)
    }

    /// Borrow the exact raw bytes for a short-lived durable queue write.
    #[must_use]
    pub fn raw_body(&self) -> &[u8] {
        &self.raw_body
    }
}

/// HMAC verifier with bounded current/previous-secret rotation overlap.
#[derive(Debug)]
pub struct WebhookVerifier {
    current: SecretBytes,
    previous: Option<SecretBytes>,
    allowed_events: HashSet<String>,
    max_body_bytes: usize,
}

impl WebhookVerifier {
    /// Construct a verifier. Secrets must contain at least 256 bits.
    ///
    /// # Errors
    ///
    /// Returns an envelope error when the event allowlist is empty. Secret
    /// length is enforced when constructing [`SecretBytes`].
    pub fn new(
        current: SecretBytes,
        previous: Option<SecretBytes>,
        allowed_events: impl IntoIterator<Item = String>,
    ) -> Result<Self, GithubAppError> {
        let allowed_events: HashSet<_> = allowed_events.into_iter().collect();
        if allowed_events.is_empty() {
            return Err(GithubAppError::InvalidWebhookEnvelope(
                "empty event allowlist",
            ));
        }
        Ok(Self {
            current,
            previous,
            allowed_events,
            max_body_bytes: MAX_WEBHOOK_BODY_BYTES,
        })
    }

    /// Authenticate a GitHub webhook without parsing JSON.
    ///
    /// # Errors
    ///
    /// Returns a webhook envelope, size, event-allowlist, or signature error
    /// when the raw request does not satisfy the complete ingress contract.
    pub fn verify(
        &self,
        delivery_id: &str,
        event: &str,
        content_type: &str,
        signature_256: &str,
        raw_body: &[u8],
    ) -> Result<VerifiedWebhook, GithubAppError> {
        if raw_body.len() > self.max_body_bytes {
            return Err(GithubAppError::WebhookTooLarge);
        }
        if delivery_id.is_empty()
            || delivery_id.len() > 128
            || !delivery_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(GithubAppError::InvalidWebhookEnvelope("delivery ID"));
        }
        if content_type
            .split(';')
            .next()
            .map(str::trim)
            .is_none_or(|mime| mime != "application/json")
        {
            return Err(GithubAppError::InvalidWebhookEnvelope("content type"));
        }
        if !self.allowed_events.contains(event) {
            return Err(GithubAppError::WebhookEventNotAllowed(event.to_owned()));
        }

        let encoded = signature_256
            .strip_prefix("sha256=")
            .ok_or(GithubAppError::WebhookSignatureInvalid)?;
        if encoded.len() != 64 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GithubAppError::WebhookSignatureInvalid);
        }
        let supplied = hex::decode(encoded).map_err(|_| GithubAppError::WebhookSignatureInvalid)?;
        let valid = std::iter::once(&self.current)
            .chain(self.previous.iter())
            .any(|secret| {
                hmac_sha256(secret.expose(), raw_body)
                    .ct_eq(&supplied)
                    .into()
            });
        if !valid {
            return Err(GithubAppError::WebhookSignatureInvalid);
        }

        Ok(VerifiedWebhook {
            delivery_id: delivery_id.to_owned(),
            event: event.to_owned(),
            body_sha256: Sha256::digest(raw_body).into(),
            raw_body: raw_body.to_vec(),
        })
    }
}

/// Minimal record that must be inserted atomically before acknowledging a webhook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryRecord {
    /// GitHub `X-GitHub-Delivery` value.
    pub delivery_id: String,
    /// Exact authenticated raw-body hash.
    pub body_sha256: [u8; 32],
    /// Allowlisted event name.
    pub event: String,
}

/// Result of an atomic durable delivery reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryReservation {
    /// First observation; processing may be enqueued.
    Accepted,
    /// Same delivery and same bytes; acknowledge without reprocessing.
    Duplicate,
    /// Same delivery ID but different bytes; emit a security alert.
    Conflict,
}

/// Durable replay-store boundary. Implementations must reserve atomically.
pub trait DeliveryReplayStore: Send + Sync {
    /// Atomically insert or compare the delivery record.
    ///
    /// # Errors
    ///
    /// Returns a sanitized storage diagnostic if the atomic transaction fails.
    fn reserve(&self, delivery: &DeliveryRecord) -> Result<DeliveryReservation, String>;
}

/// Reserve a verified delivery and convert conflicts into a stable security error.
///
/// # Errors
///
/// Returns [`GithubAppError::WebhookReplayConflict`] for delivery-ID reuse with
/// different bytes, or [`GithubAppError::ReplayStore`] on storage failure.
pub fn reserve_verified_delivery(
    store: &dyn DeliveryReplayStore,
    webhook: &VerifiedWebhook,
) -> Result<DeliveryReservation, GithubAppError> {
    let record = DeliveryRecord {
        delivery_id: webhook.delivery_id.clone(),
        body_sha256: webhook.body_sha256,
        event: webhook.event.clone(),
    };
    match store
        .reserve(&record)
        .map_err(GithubAppError::ReplayStore)?
    {
        DeliveryReservation::Conflict => Err(GithubAppError::WebhookReplayConflict {
            delivery_id: webhook.delivery_id.clone(),
        }),
        decision => Ok(decision),
    }
}

/// Small in-memory replay store intended for tests and single-process development.
#[derive(Debug, Default)]
pub struct InMemoryDeliveryReplayStore(Mutex<HashMap<String, DeliveryRecord>>);

impl DeliveryReplayStore for InMemoryDeliveryReplayStore {
    fn reserve(&self, delivery: &DeliveryRecord) -> Result<DeliveryReservation, String> {
        let mut records = self.0.lock().map_err(|_| "lock poisoned".to_owned())?;
        Ok(match records.get(&delivery.delivery_id) {
            None => {
                records.insert(delivery.delivery_id.clone(), delivery.clone());
                DeliveryReservation::Accepted
            }
            Some(existing) if existing == delivery => DeliveryReservation::Duplicate,
            Some(_) => DeliveryReservation::Conflict,
        })
    }
}

#[derive(Debug, Serialize)]
struct AppClaims {
    iss: String,
    iat: i64,
    exp: i64,
}

/// Short-lived App bearer JWT. Debug output is always redacted.
pub struct AppJwt(String);

impl AppJwt {
    /// Issue an RS256 GitHub App JWT at a supplied clock value.
    ///
    /// The private key remains caller-owned and should come from a Hub secret
    /// manager. The token lifetime is nine minutes and `iat` is skewed by 60s.
    ///
    /// # Errors
    ///
    /// Returns [`GithubAppError::Jwt`] if the PEM is not a usable RSA key or the
    /// RS256 token cannot be encoded.
    pub fn issue(
        app_id: AppId,
        rsa_private_key_pem: &SecretBytes,
        now: OffsetDateTime,
    ) -> Result<Self, GithubAppError> {
        let key = EncodingKey::from_rsa_pem(rsa_private_key_pem.expose())?;
        let claims = AppClaims {
            iss: app_id.get().to_string(),
            iat: (now - Duration::seconds(60)).unix_timestamp(),
            exp: (now + Duration::minutes(9)).unix_timestamp(),
        };
        let mut header = Header::new(Algorithm::RS256);
        header.typ = Some("JWT".to_owned());
        Ok(Self(jsonwebtoken::encode(&header, &claims, &key)?))
    }

    fn expose(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    fn test_value() -> Self {
        Self("test.jwt.signature".to_owned())
    }
}

impl fmt::Debug for AppJwt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AppJwt([REDACTED])")
    }
}

impl Drop for AppJwt {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// A request passed to the injectable GitHub HTTP transport.
pub struct GithubApiRequest {
    /// HTTP method.
    pub method: reqwest::Method,
    /// Fully constructed and allowlisted API URL.
    pub url: Url,
    /// Request headers. Authorization is redacted by `Debug`.
    pub headers: Vec<(String, String)>,
    /// Optional JSON bytes.
    pub body: Option<Vec<u8>>,
}

impl fmt::Debug for GithubApiRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("GithubApiRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("header_names", &header_names)
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .finish()
    }
}

/// Raw response supplied by a production or mocked transport.
#[derive(Clone, Debug)]
pub struct GithubApiResponse {
    /// HTTP status code.
    pub status: u16,
    /// Lowercase or case-insensitive response headers.
    pub headers: Vec<(String, String)>,
    /// Strictly bounded response bytes (the production transport enforces 2 MiB).
    pub body: Vec<u8>,
}

impl GithubApiResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Injectable provider transport for deterministic tests.
#[async_trait]
pub trait GithubTransport: Send + Sync {
    /// Send one already-allowlisted GitHub API request.
    async fn send(&self, request: GithubApiRequest) -> Result<GithubApiResponse, GithubAppError>;
}

/// Rustls-backed public GitHub transport with a second URL allowlist check.
#[derive(Clone, Debug)]
pub struct ReqwestGithubTransport {
    client: reqwest::Client,
}

impl ReqwestGithubTransport {
    /// Build a provider client with redirects disabled and bounded timeouts.
    ///
    /// # Errors
    ///
    /// Returns [`GithubAppError::Transport`] if the TLS HTTP client cannot be built.
    pub fn new() -> Result<Self, GithubAppError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| GithubAppError::Transport(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl GithubTransport for ReqwestGithubTransport {
    async fn send(&self, request: GithubApiRequest) -> Result<GithubApiResponse, GithubAppError> {
        ensure_public_github_api_url(&request.url)?;
        let mut builder = self.client.request(request.method, request.url);
        for (name, value) in request.headers {
            builder = builder.header(&name, &value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| GithubAppError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let body = response
            .bytes()
            .await
            .map_err(|error| GithubAppError::Transport(error.to_string()))?;
        if body.len() > 2 * 1024 * 1024 {
            return Err(GithubAppError::Provider {
                status,
                message: "response exceeded 2 MiB".to_owned(),
            });
        }
        Ok(GithubApiResponse {
            status,
            headers,
            body: body.to_vec(),
        })
    }
}

/// Reject any endpoint that is not HTTPS on the exact public GitHub API origin.
///
/// # Errors
///
/// Returns [`GithubAppError::UrlNotAllowlisted`] for every non-public-GitHub origin.
pub fn ensure_public_github_api_url(url: &Url) -> Result<(), GithubAppError> {
    let valid = url.scheme() == "https"
        && url.host_str() == Some("api.github.com")
        && matches!(url.port(), None | Some(443))
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none();
    if valid {
        Ok(())
    } else {
        Err(GithubAppError::UrlNotAllowlisted)
    }
}

/// User-visible retry classification derived from provider status and headers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryDisposition {
    /// The operation succeeded and needs no retry.
    Success,
    /// GitHub primary or secondary rate limiting; queue until the supplied time.
    RateLimited {
        /// Provider `Retry-After`, if supplied.
        retry_after_seconds: Option<u64>,
        /// Provider primary rate-limit reset epoch, if supplied.
        reset_epoch_seconds: Option<i64>,
    },
    /// A transient outage. Retry only idempotent operations with bounded jitter.
    Transient,
    /// Authentication/installation state must be reconciled before retry.
    Reauthenticate,
    /// Request/policy/permission failure; retrying unchanged input is unsafe.
    Permanent,
}

/// Classify a response without initiating an unsafe automatic mutation retry.
#[must_use]
pub fn classify_response(response: &GithubApiResponse) -> RetryDisposition {
    if (200..300).contains(&response.status) {
        return RetryDisposition::Success;
    }
    let retry_after_seconds = response
        .header("retry-after")
        .and_then(|value| u64::from_str(value).ok());
    let remaining_zero = response.header("x-ratelimit-remaining") == Some("0");
    if response.status == 429
        || (response.status == 403 && (remaining_zero || retry_after_seconds.is_some()))
    {
        return RetryDisposition::RateLimited {
            retry_after_seconds,
            reset_epoch_seconds: response
                .header("x-ratelimit-reset")
                .and_then(|value| i64::from_str(value).ok()),
        };
    }
    match response.status {
        401 => RetryDisposition::Reauthenticate,
        408 | 500 | 502 | 503 | 504 => RetryDisposition::Transient,
        _ => RetryDisposition::Permanent,
    }
}

/// A repository-scoped installation token held only in process memory.
#[derive(Debug)]
pub struct InstallationToken {
    /// App installation which minted the token.
    pub installation_id: InstallationId,
    /// Repository scope requested during minting.
    pub repository_id: RepositoryId,
    /// Provider expiry; cache only until this value minus 60 seconds.
    pub expires_at: OffsetDateTime,
    token: SecretString,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
    expires_at: String,
}

/// Authoritative repository identity returned from `/repositories/{id}`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryIdentity {
    /// Provider numeric ID.
    pub id: RepositoryId,
    /// Current owner/name cache, verified together with the numeric ID.
    pub full_name: String,
    /// Archived repositories are read-only for `NeuMan` mutations.
    pub archived: bool,
}

#[derive(Debug, Deserialize)]
struct RepositoryResponse {
    id: u64,
    full_name: String,
    #[serde(default)]
    archived: bool,
}

/// GitHub check-run conclusion supported by SPEC-12.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    /// Gates passed.
    Success,
    /// Deterministic validation or build failure.
    Failure,
    /// Check does not apply.
    Neutral,
    /// User or system cancellation.
    Cancelled,
    /// Deadline exceeded.
    TimedOut,
    /// Approval or documented manual action is required.
    ActionRequired,
    /// Explicit policy skip.
    Skipped,
}

/// Bounded check-run output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckOutput {
    /// Short result title.
    pub title: String,
    /// Markdown summary without credentials or private URLs.
    pub summary: String,
    /// Optional bounded details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Input for a completed `NeuMan` check run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedCheckRun {
    /// Stable reference check name.
    pub name: String,
    /// Exact 40-character Git commit SHA.
    pub head_sha: String,
    /// Outcome with no implication that deployment occurred.
    pub conclusion: CheckConclusion,
    /// Bounded, sanitized output.
    pub output: CheckOutput,
    /// Optional authorized Hub/local handoff URL.
    pub details_url: Option<Url>,
    /// Caller-generated idempotency/external identifier.
    pub external_id: String,
}

impl CompletedCheckRun {
    fn validate(&self) -> Result<(), GithubAppError> {
        const ALLOWED_PREFIXES: [&str; 6] = [
            "NeuMan / Configuration",
            "NeuMan / Ownership",
            "NeuMan / Art Validation",
            "NeuMan / Build ",
            "NeuMan / Staging",
            "NeuMan / Release Readiness",
        ];
        if !ALLOWED_PREFIXES
            .iter()
            .any(|prefix| self.name.starts_with(prefix))
            || self.name.len() > 100
            || self.head_sha.len() != 40
            || !self.head_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.output.title.len() > 255
            || self.output.summary.len() > 65_535
            || self
                .output
                .text
                .as_ref()
                .is_some_and(|text| text.len() > 65_535)
            || self.external_id.is_empty()
            || self.external_id.len() > 128
        {
            return Err(GithubAppError::Provider {
                status: 400,
                message: "invalid check-run input".to_owned(),
            });
        }
        if self
            .details_url
            .as_ref()
            .is_some_and(|url| url.scheme() != "https")
        {
            return Err(GithubAppError::Provider {
                status: 400,
                message: "check details URL must use HTTPS".to_owned(),
            });
        }
        Ok(())
    }
}

/// Provider check-run result.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct CheckRunResult {
    /// Provider check-run numeric ID.
    pub id: u64,
    /// Provider status.
    pub status: String,
    /// Provider conclusion, if completed.
    pub conclusion: Option<String>,
}

/// Public GitHub API operations. All paths are constructed locally.
#[derive(Debug)]
pub struct GithubApiClient<T> {
    transport: T,
}

impl<T: GithubTransport> GithubApiClient<T> {
    /// Construct a client around a production or mocked transport.
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Mint a token scoped to exactly the bound repository numeric ID.
    ///
    /// # Errors
    ///
    /// Returns a validation, transport, provider, JSON, or expiry error. No
    /// token is returned unless the repository-scoped exchange succeeds.
    pub async fn mint_repository_token(
        &self,
        binding: &RepositoryBinding,
        app_jwt: &AppJwt,
    ) -> Result<InstallationToken, GithubAppError> {
        binding.validate()?;
        let url = endpoint(&format!(
            "app/installations/{}/access_tokens",
            binding.installation_id.get()
        ))?;
        let body = serde_json::to_vec(&serde_json::json!({
            "repository_ids": [binding.repository_id.get()]
        }))?;
        let response = self
            .transport
            .send(api_request(
                reqwest::Method::POST,
                url,
                app_jwt.expose(),
                Some(body),
            ))
            .await?;
        require_success(&response)?;
        let parsed: TokenResponse = serde_json::from_slice(&response.body)?;
        let expires_at = OffsetDateTime::parse(&parsed.expires_at, &Rfc3339).map_err(|_| {
            GithubAppError::Provider {
                status: response.status,
                message: "invalid installation-token expiry".to_owned(),
            }
        })?;
        if expires_at <= OffsetDateTime::now_utc() + Duration::seconds(60) {
            return Err(GithubAppError::Provider {
                status: response.status,
                message: "installation token expires too soon".to_owned(),
            });
        }
        Ok(InstallationToken {
            installation_id: binding.installation_id,
            repository_id: binding.repository_id,
            expires_at,
            token: SecretString::from_provider(parsed.token)?,
        })
    }

    /// Fetch repository metadata by numeric ID and reject identity mismatches.
    ///
    /// # Errors
    ///
    /// Returns a binding, transport, provider, JSON, repository-name, or
    /// repository numeric-ID mismatch error.
    pub async fn verify_repository(
        &self,
        binding: &RepositoryBinding,
        token: &InstallationToken,
    ) -> Result<RepositoryIdentity, GithubAppError> {
        ensure_token_binding(binding, token)?;
        let url = endpoint(&format!("repositories/{}", binding.repository_id.get()))?;
        let response = self
            .transport
            .send(api_request(
                reqwest::Method::GET,
                url,
                token.token.expose(),
                None,
            ))
            .await?;
        require_success(&response)?;
        let parsed: RepositoryResponse = serde_json::from_slice(&response.body)?;
        if parsed.id != binding.repository_id.get() {
            return Err(GithubAppError::RepositoryMismatch {
                expected: binding.repository_id.get(),
                observed: parsed.id,
            });
        }
        validate_full_name(&parsed.full_name)?;
        Ok(RepositoryIdentity {
            id: binding.repository_id,
            full_name: parsed.full_name,
            archived: parsed.archived,
        })
    }

    /// Create a completed check run for an already numeric-ID-verified repository.
    ///
    /// # Errors
    ///
    /// Returns a binding, archived-repository, input, transport, provider, or
    /// response-decoding error.
    pub async fn create_check_run(
        &self,
        binding: &RepositoryBinding,
        token: &InstallationToken,
        repository: &RepositoryIdentity,
        check: &CompletedCheckRun,
    ) -> Result<CheckRunResult, GithubAppError> {
        ensure_mutation_binding(binding, token, repository)?;
        check.validate()?;
        let url = repository_endpoint(repository, "check-runs")?;
        let body = completed_check_body(check)?;
        let response = self
            .transport
            .send(api_request(
                reqwest::Method::POST,
                url,
                token.token.expose(),
                Some(body),
            ))
            .await?;
        require_success(&response)?;
        Ok(serde_json::from_slice(&response.body)?)
    }

    /// Update an existing check run, retaining repository binding validation.
    ///
    /// # Errors
    ///
    /// Returns a binding, archived-repository, identifier, input, transport,
    /// provider, or response-decoding error.
    pub async fn update_check_run(
        &self,
        binding: &RepositoryBinding,
        token: &InstallationToken,
        repository: &RepositoryIdentity,
        check_run_id: u64,
        check: &CompletedCheckRun,
    ) -> Result<CheckRunResult, GithubAppError> {
        ensure_mutation_binding(binding, token, repository)?;
        check.validate()?;
        if check_run_id == 0 {
            return Err(GithubAppError::InvalidIdentifier { kind: "check run" });
        }
        let url = repository_endpoint(repository, &format!("check-runs/{check_run_id}"))?;
        let response = self
            .transport
            .send(api_request(
                reqwest::Method::PATCH,
                url,
                token.token.expose(),
                Some(completed_check_body(check)?),
            ))
            .await?;
        require_success(&response)?;
        Ok(serde_json::from_slice(&response.body)?)
    }
}

fn endpoint(path: &str) -> Result<Url, GithubAppError> {
    let mut url = Url::parse(PUBLIC_API_ORIGIN).map_err(|_| GithubAppError::UrlNotAllowlisted)?;
    url.set_path(path);
    ensure_public_github_api_url(&url)?;
    Ok(url)
}

fn repository_endpoint(
    repository: &RepositoryIdentity,
    suffix: &str,
) -> Result<Url, GithubAppError> {
    let (owner, name) = repository
        .full_name
        .split_once('/')
        .ok_or(GithubAppError::InvalidRepositoryName)?;
    let mut url = Url::parse(PUBLIC_API_ORIGIN).map_err(|_| GithubAppError::UrlNotAllowlisted)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| GithubAppError::UrlNotAllowlisted)?;
        segments.clear().push("repos").push(owner).push(name);
        for segment in suffix.split('/') {
            segments.push(segment);
        }
    }
    ensure_public_github_api_url(&url)?;
    Ok(url)
}

fn api_request(
    method: reqwest::Method,
    url: Url,
    bearer: &str,
    body: Option<Vec<u8>>,
) -> GithubApiRequest {
    let mut headers = vec![
        (
            "accept".to_owned(),
            "application/vnd.github+json".to_owned(),
        ),
        ("authorization".to_owned(), format!("Bearer {bearer}")),
        ("x-github-api-version".to_owned(), API_VERSION.to_owned()),
        ("user-agent".to_owned(), DEFAULT_USER_AGENT.to_owned()),
    ];
    if body.is_some() {
        headers.push(("content-type".to_owned(), "application/json".to_owned()));
    }
    GithubApiRequest {
        method,
        url,
        headers,
        body,
    }
}

fn require_success(response: &GithubApiResponse) -> Result<(), GithubAppError> {
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    let message = serde_json::from_slice::<serde_json::Value>(&response.body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "provider request failed".to_owned());
    Err(GithubAppError::Provider {
        status: response.status,
        message,
    })
}

fn ensure_token_binding(
    binding: &RepositoryBinding,
    token: &InstallationToken,
) -> Result<(), GithubAppError> {
    if binding.installation_id != token.installation_id
        || binding.repository_id != token.repository_id
    {
        return Err(GithubAppError::RepositoryMismatch {
            expected: binding.repository_id.get(),
            observed: token.repository_id.get(),
        });
    }
    Ok(())
}

fn ensure_mutation_binding(
    binding: &RepositoryBinding,
    token: &InstallationToken,
    repository: &RepositoryIdentity,
) -> Result<(), GithubAppError> {
    ensure_token_binding(binding, token)?;
    if repository.id != binding.repository_id {
        return Err(GithubAppError::RepositoryMismatch {
            expected: binding.repository_id.get(),
            observed: repository.id.get(),
        });
    }
    if repository.archived {
        return Err(GithubAppError::Provider {
            status: 409,
            message: "repository is archived".to_owned(),
        });
    }
    Ok(())
}

fn validate_full_name(full_name: &str) -> Result<(), GithubAppError> {
    let Some((owner, repository)) = full_name.split_once('/') else {
        return Err(GithubAppError::InvalidRepositoryName);
    };
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || full_name.len() > 201
        || !full_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(GithubAppError::InvalidRepositoryName);
    }
    Ok(())
}

fn completed_check_body(check: &CompletedCheckRun) -> Result<Vec<u8>, GithubAppError> {
    Ok(serde_json::to_vec(&serde_json::json!({
        "name": check.name,
        "head_sha": check.head_sha,
        "status": "completed",
        "conclusion": check.conclusion,
        "output": check.output,
        "details_url": check.details_url,
        "external_id": check.external_id,
    }))?)
}

/// Authenticated payload embedded in a PR body comment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HiddenMarkerPayload {
    /// Marker schema version, currently exactly one.
    pub version: u8,
    /// Project identity.
    pub project_id: String,
    /// Authoritative repository numeric ID.
    pub repository_id: RepositoryId,
    /// Optional proposal identity.
    pub proposal_id: Option<String>,
    /// Optional build identity.
    pub build_id: Option<String>,
    /// Optional release identity.
    pub release_id: Option<String>,
    /// Sorted immutable `algorithm:digest` values covered by the signature.
    pub immutable_hashes: Vec<String>,
}

impl HiddenMarkerPayload {
    fn validate(&self) -> Result<(), GithubAppError> {
        if self.version != 1
            || self.project_id.len() < 8
            || self.project_id.len() > 128
            || self.immutable_hashes.is_empty()
            || self.immutable_hashes.len() > 128
            || self
                .immutable_hashes
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self
                .immutable_hashes
                .iter()
                .any(|hash| !valid_immutable_hash(hash))
            || [&self.proposal_id, &self.build_id, &self.release_id]
                .into_iter()
                .flatten()
                .any(|id| id.is_empty() || id.len() > 128)
        {
            return Err(GithubAppError::InvalidMarker("payload policy"));
        }
        Ok(())
    }
}

fn valid_immutable_hash(value: &str) -> bool {
    if let Some(digest) = value.strip_prefix("sha256:") {
        return digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if let Some(digest) = value.strip_prefix("b3-256:") {
        return digest.len() == 52
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    }
    false
}

/// Generate an exact authenticated hidden comment using RFC 8785 canonical JSON.
///
/// # Errors
///
/// Returns [`GithubAppError::InvalidMarker`] when the payload violates marker
/// policy or canonical serialization fails.
pub fn generate_hidden_marker(
    payload: &HiddenMarkerPayload,
    marker_key: &SecretBytes,
) -> Result<String, GithubAppError> {
    payload.validate()?;
    let canonical =
        serde_jcs::to_vec(payload).map_err(|_| GithubAppError::InvalidMarker("canonical JSON"))?;
    let signature = hmac_sha256(marker_key.expose(), &canonical);
    Ok(format!(
        "{MARKER_PREFIX}{}:{}{MARKER_SUFFIX}",
        URL_SAFE_NO_PAD.encode(canonical),
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

/// Validate one exact hidden marker and return its authenticated payload.
///
/// # Errors
///
/// Returns [`GithubAppError::InvalidMarker`] for an invalid envelope, encoding,
/// HMAC, schema, canonical representation, identifier, or immutable hash.
pub fn validate_hidden_marker(
    marker: &str,
    marker_key: &SecretBytes,
) -> Result<HiddenMarkerPayload, GithubAppError> {
    let encoded = marker
        .strip_prefix(MARKER_PREFIX)
        .and_then(|value| value.strip_suffix(MARKER_SUFFIX))
        .ok_or(GithubAppError::InvalidMarker("envelope"))?;
    let (payload, signature) = encoded
        .split_once(':')
        .ok_or(GithubAppError::InvalidMarker("segments"))?;
    if signature.contains(':') {
        return Err(GithubAppError::InvalidMarker("segments"));
    }
    let canonical = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| GithubAppError::InvalidMarker("payload encoding"))?;
    let supplied = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| GithubAppError::InvalidMarker("signature encoding"))?;
    if supplied.len() != 32
        || !bool::from(hmac_sha256(marker_key.expose(), &canonical).ct_eq(&supplied))
    {
        return Err(GithubAppError::InvalidMarker("signature"));
    }
    let parsed: HiddenMarkerPayload = serde_json::from_slice(&canonical)
        .map_err(|_| GithubAppError::InvalidMarker("payload JSON"))?;
    parsed.validate()?;
    let recanonical =
        serde_jcs::to_vec(&parsed).map_err(|_| GithubAppError::InvalidMarker("canonical JSON"))?;
    if recanonical != canonical {
        return Err(GithubAppError::InvalidMarker("non-canonical payload"));
    }
    Ok(parsed)
}

/// Extract exactly one `NeuMan` marker from a larger PR body and authenticate it.
///
/// # Errors
///
/// Returns [`GithubAppError::InvalidMarker`] when the body contains zero,
/// multiple, incomplete, or unauthenticated markers.
pub fn extract_hidden_marker(
    body: &str,
    marker_key: &SecretBytes,
) -> Result<HiddenMarkerPayload, GithubAppError> {
    let start = body
        .find(MARKER_PREFIX)
        .ok_or(GithubAppError::InvalidMarker("missing"))?;
    let tail = &body[start..];
    let end = tail
        .find(MARKER_SUFFIX)
        .map(|index| index + MARKER_SUFFIX.len())
        .ok_or(GithubAppError::InvalidMarker("unterminated"))?;
    if body[start + end..].contains(MARKER_PREFIX) {
        return Err(GithubAppError::InvalidMarker("multiple markers"));
    }
    validate_hidden_marker(&tail[..end], marker_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Arc};

    fn secret(value: u8) -> SecretBytes {
        SecretBytes::new([value; 32], "test").expect("test secret")
    }

    fn verifier() -> WebhookVerifier {
        WebhookVerifier::new(secret(7), None, ["ping".to_owned()]).expect("verifier")
    }

    #[test]
    fn hmac_matches_github_documented_vector() {
        let digest = hmac_sha256(b"It's a Secret to Everybody", b"Hello, World!");
        assert_eq!(
            hex::encode(digest),
            "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
    }

    #[test]
    fn signature_is_checked_before_json_parsing() {
        let raw = b"this is deliberately not JSON";
        let signature = format!("sha256={}", hex::encode(hmac_sha256(&[7; 32], raw)));
        let verified = verifier()
            .verify("delivery-1", "ping", "application/json", &signature, raw)
            .expect("authenticated bytes");
        assert!(matches!(
            verified.parse_json::<serde_json::Value>(),
            Err(GithubAppError::InvalidJson(_))
        ));
        assert!(matches!(
            verifier().verify(
                "delivery-2",
                "ping",
                "application/json",
                "sha256=0000000000000000000000000000000000000000000000000000000000000000",
                b"{}"
            ),
            Err(GithubAppError::WebhookSignatureInvalid)
        ));
    }

    #[test]
    fn replay_same_hash_is_idempotent_and_different_hash_conflicts() {
        let store = InMemoryDeliveryReplayStore::default();
        let make = |raw: &[u8]| {
            let signature = format!("sha256={}", hex::encode(hmac_sha256(&[7; 32], raw)));
            verifier()
                .verify("delivery-1", "ping", "application/json", &signature, raw)
                .expect("verified")
        };
        let first = make(b"{}");
        assert_eq!(
            reserve_verified_delivery(&store, &first).expect("first"),
            DeliveryReservation::Accepted
        );
        assert_eq!(
            reserve_verified_delivery(&store, &first).expect("duplicate"),
            DeliveryReservation::Duplicate
        );
        assert!(matches!(
            reserve_verified_delivery(&store, &make(br#"{"different":true}"#)),
            Err(GithubAppError::WebhookReplayConflict { .. })
        ));
    }

    #[test]
    fn api_origin_allowlist_rejects_ssrf_shapes() {
        assert!(
            ensure_public_github_api_url(
                &Url::parse("https://api.github.com/repositories/1").unwrap()
            )
            .is_ok()
        );
        for unsafe_url in [
            "http://api.github.com/repositories/1",
            "https://api.github.com.evil.test/repositories/1",
            "https://evil.test/?next=https://api.github.com",
            "https://user@api.github.com/repositories/1",
            "https://api.github.com:8443/repositories/1",
        ] {
            assert!(ensure_public_github_api_url(&Url::parse(unsafe_url).unwrap()).is_err());
        }
    }

    fn marker_payload() -> HiddenMarkerPayload {
        HiddenMarkerPayload {
            version: 1,
            project_id: "prj_12345678".to_owned(),
            repository_id: RepositoryId::new(42).unwrap(),
            proposal_id: Some("prop_123".to_owned()),
            build_id: Some("bld_123".to_owned()),
            release_id: None,
            immutable_hashes: vec![format!("sha256:{}", "a".repeat(64))],
        }
    }

    #[test]
    fn hidden_marker_round_trips_and_tampering_fails() {
        let key = secret(9);
        let marker = generate_hidden_marker(&marker_payload(), &key).expect("marker");
        assert_eq!(
            validate_hidden_marker(&marker, &key).unwrap(),
            marker_payload()
        );
        let mut tampered = marker.into_bytes();
        let index = MARKER_PREFIX.len() + 4;
        tampered[index] = if tampered[index] == b'A' { b'B' } else { b'A' };
        assert!(validate_hidden_marker(std::str::from_utf8(&tampered).unwrap(), &key).is_err());
    }

    #[derive(Clone, Debug)]
    struct MockTransport {
        responses: Arc<Mutex<VecDeque<GithubApiResponse>>>,
        requests: Arc<Mutex<Vec<GithubApiRequest>>>,
    }

    impl MockTransport {
        fn new(responses: Vec<GithubApiResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl GithubTransport for MockTransport {
        async fn send(
            &self,
            request: GithubApiRequest,
        ) -> Result<GithubApiResponse, GithubAppError> {
            ensure_public_github_api_url(&request.url)?;
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| GithubAppError::Transport("mock response exhausted".to_owned()))
        }
    }

    fn response(status: u16, body: serde_json::Value) -> GithubApiResponse {
        GithubApiResponse {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn binding() -> RepositoryBinding {
        RepositoryBinding {
            project_id: "prj_12345678".to_owned(),
            installation_id: InstallationId::new(7).unwrap(),
            repository_id: RepositoryId::new(42).unwrap(),
            display_full_name: None,
        }
    }

    #[tokio::test]
    async fn mocked_token_and_repository_responses_remain_numeric_id_scoped() {
        let expiry = (OffsetDateTime::now_utc() + Duration::hours(1))
            .format(&Rfc3339)
            .unwrap();
        let transport = MockTransport::new(vec![
            response(
                201,
                serde_json::json!({"token": "ghs_abcdefghijklmnopqrstuvwxyz123456", "expires_at": expiry}),
            ),
            response(
                200,
                serde_json::json!({"id": 42, "full_name": "owner/repository", "archived": false}),
            ),
        ]);
        let requests = Arc::clone(&transport.requests);
        let client = GithubApiClient::new(transport);
        let binding = binding();
        let token = client
            .mint_repository_token(&binding, &AppJwt::test_value())
            .await
            .expect("token");
        let repository = client
            .verify_repository(&binding, &token)
            .await
            .expect("repository");
        assert_eq!(repository.id, RepositoryId::new(42).unwrap());

        let requests = requests.lock().unwrap();
        let mint_body: serde_json::Value =
            serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(mint_body["repository_ids"], serde_json::json!([42]));
        assert_eq!(requests[1].url.path(), "/repositories/42");
        assert!(!format!("{:?}", requests[0]).contains("test.jwt.signature"));
    }

    #[tokio::test]
    async fn mocked_check_run_uses_verified_owner_name() {
        let transport = MockTransport::new(vec![response(
            201,
            serde_json::json!({"id": 99, "status": "completed", "conclusion": "success"}),
        )]);
        let requests = Arc::clone(&transport.requests);
        let client = GithubApiClient::new(transport);
        let binding = binding();
        let token = InstallationToken {
            installation_id: binding.installation_id,
            repository_id: binding.repository_id,
            expires_at: OffsetDateTime::now_utc() + Duration::hours(1),
            token: SecretString::from_provider("ghs_abcdefghijklmnopqrstuvwxyz123456".to_owned())
                .unwrap(),
        };
        let repository = RepositoryIdentity {
            id: binding.repository_id,
            full_name: "owner/repository".to_owned(),
            archived: false,
        };
        let check = CompletedCheckRun {
            name: "NeuMan / Configuration".to_owned(),
            head_sha: "a".repeat(40),
            conclusion: CheckConclusion::Success,
            output: CheckOutput {
                title: "Configuration valid".to_owned(),
                summary: "All deterministic configuration gates passed.".to_owned(),
                text: None,
            },
            details_url: Some(Url::parse("https://hub.example.test/check/1").unwrap()),
            external_id: "chk_12345678".to_owned(),
        };
        let result = client
            .create_check_run(&binding, &token, &repository, &check)
            .await
            .expect("check");
        assert_eq!(result.id, 99);
        assert_eq!(
            requests.lock().unwrap()[0].url.path(),
            "/repos/owner/repository/check-runs"
        );
    }

    #[test]
    fn rate_limit_classification_respects_provider_headers() {
        let limited = GithubApiResponse {
            status: 403,
            headers: vec![
                ("x-ratelimit-remaining".to_owned(), "0".to_owned()),
                ("x-ratelimit-reset".to_owned(), "1700000000".to_owned()),
            ],
            body: Vec::new(),
        };
        assert_eq!(
            classify_response(&limited),
            RetryDisposition::RateLimited {
                retry_after_seconds: None,
                reset_epoch_seconds: Some(1_700_000_000),
            }
        );
    }
}
