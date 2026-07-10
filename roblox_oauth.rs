//! Roblox public-client OAuth refresh rotation.
//!
//! This module performs no persistence and accepts no client secret. Callers
//! must atomically persist the returned rotating refresh token before replacing
//! their previous durable record.

#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;
use url::Url;

/// Fixed Roblox OpenID Connect discovery endpoint.
pub const ROBLOX_OAUTH_DISCOVERY_URL: &str =
    "https://apis.roblox.com/oauth/.well-known/openid-configuration";
/// Exact accepted Roblox OAuth issuer.
pub const ROBLOX_OAUTH_ISSUER: &str = "https://apis.roblox.com/oauth/";
/// Exact accepted HTTPS endpoint origin.
pub const ROBLOX_OAUTH_ORIGIN: &str = "https://apis.roblox.com";
/// Maximum body accepted from discovery, token, userinfo, or JWKS endpoints.
pub const MAX_OAUTH_RESPONSE_BYTES: usize = 1024 * 1024;
/// Default minimum useful lifetime for a newly refreshed access token.
pub const MIN_ACCESS_TOKEN_LIFETIME_SECONDS: u64 = 60;

const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_SUBJECT_BYTES: usize = 512;
const MAX_SCOPE_COUNT: usize = 128;
const MAX_JWK_COUNT: usize = 128;
const CLOCK_SKEW_SECONDS: u64 = 60;

/// Result type for OAuth refresh operations.
pub type Result<T> = std::result::Result<T, RobloxOAuthError>;

/// Stable error classification for caller state transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RobloxOAuthErrorKind {
    /// The rotating credential is no longer usable; interactive authorization is required.
    ReauthenticationRequired,
    /// Provider/network failure where a later identical attempt may succeed.
    ProviderUnavailable,
    /// Provider data violated the expected OAuth/OIDC contract.
    InvalidProviderResponse,
    /// Local public-client configuration or refresh context is invalid.
    InvalidRequest,
    /// Signature, origin, identity, or token correlation failed.
    SecurityViolation,
}

/// Redacted OAuth error. Provider bodies and secrets are never embedded.
#[derive(Clone, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct RobloxOAuthError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// State transition classification.
    pub kind: RobloxOAuthErrorKind,
    /// Safe static explanation.
    pub message: &'static str,
    /// Whether a bounded later retry of unchanged input may succeed.
    pub retryable: bool,
}

impl fmt::Debug for RobloxOAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RobloxOAuthError")
            .field("code", &self.code)
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("retryable", &self.retryable)
            .finish()
    }
}

impl RobloxOAuthError {
    const fn new(
        code: &'static str,
        kind: RobloxOAuthErrorKind,
        message: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            kind,
            message,
            retryable,
        }
    }

    const fn reauthentication_required() -> Self {
        Self::new(
            "ROBLOX_OAUTH_REAUTH_REQUIRED",
            RobloxOAuthErrorKind::ReauthenticationRequired,
            "Roblox rejected the rotating refresh token; interactive authorization is required",
            false,
        )
    }
}

/// In-memory OAuth secret with deliberately redacted formatting and no serde support.
#[derive(Clone, Eq, PartialEq)]
pub struct OAuthSecret(String);

impl OAuthSecret {
    /// Validate and wrap an access, refresh, or ID token held by the backend.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SECRET_BYTES || value.contains(['\r', '\n', '\0'])
        {
            return Err(RobloxOAuthError::new(
                "ROBLOX_OAUTH_SECRET_INVALID",
                RobloxOAuthErrorKind::InvalidRequest,
                "OAuth secret was empty, oversized, or malformed",
                false,
            ));
        }
        Ok(Self(value))
    }

    /// Explicitly expose bytes to the transport or caller's protected persistence boundary.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OAuthSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OAuthSecret([REDACTED])")
    }
}

/// Validated public-client configuration. There is intentionally no secret field.
#[derive(Clone, Debug)]
pub struct RobloxPublicClientConfig {
    client_id: String,
    required_scopes: BTreeSet<String>,
    minimum_access_lifetime_seconds: u64,
}

impl RobloxPublicClientConfig {
    /// Create a public Roblox OAuth client with required effective scopes.
    pub fn new<I, S>(client_id: impl Into<String>, required_scopes: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let client_id = client_id.into();
        if client_id.len() < 3
            || client_id.len() > 256
            || !client_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RobloxOAuthError::new(
                "ROBLOX_OAUTH_CLIENT_ID_INVALID",
                RobloxOAuthErrorKind::InvalidRequest,
                "Roblox public client ID is malformed",
                false,
            ));
        }
        let required_scopes = validate_required_scopes(required_scopes)?;
        Ok(Self {
            client_id,
            required_scopes,
            minimum_access_lifetime_seconds: MIN_ACCESS_TOKEN_LIFETIME_SECONDS,
        })
    }

    /// Use the recommended v1 identity and universe-read scopes.
    pub fn recommended(client_id: impl Into<String>) -> Result<Self> {
        Self::new(client_id, ["openid", "profile", "universe:read"])
    }

    /// Set the minimum acceptable newly issued access-token lifetime.
    pub fn with_minimum_access_lifetime(mut self, seconds: u64) -> Result<Self> {
        if !(MIN_ACCESS_TOKEN_LIFETIME_SECONDS..=86_400).contains(&seconds) {
            return Err(RobloxOAuthError::new(
                "ROBLOX_OAUTH_LIFETIME_INVALID",
                RobloxOAuthErrorKind::InvalidRequest,
                "Minimum access-token lifetime is outside the supported range",
                false,
            ));
        }
        self.minimum_access_lifetime_seconds = seconds;
        Ok(self)
    }

    /// Public application identifier.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Required effective scopes.
    #[must_use]
    pub fn required_scopes(&self) -> &BTreeSet<String> {
        &self.required_scopes
    }
}

/// Existing protected credential and immutable account binding used for one refresh.
pub struct RobloxRefreshContext {
    refresh_token: OAuthSecret,
    expected_subject: String,
}

impl RobloxRefreshContext {
    /// Construct context restored from protected storage.
    pub fn new(refresh_token: OAuthSecret, expected_subject: impl Into<String>) -> Result<Self> {
        let expected_subject = expected_subject.into();
        validate_subject(&expected_subject, RobloxOAuthErrorKind::InvalidRequest)?;
        Ok(Self {
            refresh_token,
            expected_subject,
        })
    }

    /// Existing rotating refresh token.
    #[must_use]
    pub fn refresh_token(&self) -> &OAuthSecret {
        &self.refresh_token
    }

    /// Immutable Roblox `sub` from the original authenticated account.
    #[must_use]
    pub fn expected_subject(&self) -> &str {
        &self.expected_subject
    }
}

impl fmt::Debug for RobloxRefreshContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RobloxRefreshContext")
            .field("refresh_token", &self.refresh_token)
            .field("expected_subject", &self.expected_subject)
            .finish()
    }
}

/// Validated Roblox user information associated with the refreshed token.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxUserInfo {
    /// Immutable Roblox OAuth subject.
    pub sub: String,
    /// Display name when returned.
    pub name: Option<String>,
    /// Preferred username when returned.
    pub preferred_username: Option<String>,
}

/// Successfully rotated, fully validated token result.
///
/// This type intentionally has no serde implementation. The caller chooses an
/// OS credential-vault transaction and must persist `refresh_token` atomically.
#[derive(Clone)]
pub struct RotatedRobloxTokens {
    access_token: OAuthSecret,
    refresh_token: OAuthSecret,
    id_token: Option<OAuthSecret>,
    expires_in_seconds: u64,
    scopes: BTreeSet<String>,
    user: RobloxUserInfo,
}

impl RotatedRobloxTokens {
    /// Newly issued bearer access token.
    #[must_use]
    pub fn access_token(&self) -> &OAuthSecret {
        &self.access_token
    }

    /// Newly issued one-time rotating refresh token.
    #[must_use]
    pub fn refresh_token(&self) -> &OAuthSecret {
        &self.refresh_token
    }

    /// Optional newly issued signed ID token.
    #[must_use]
    pub fn id_token(&self) -> Option<&OAuthSecret> {
        self.id_token.as_ref()
    }

    /// Provider response lifetime for the access token.
    #[must_use]
    pub fn expires_in_seconds(&self) -> u64 {
        self.expires_in_seconds
    }

    /// Effective scope set returned by Roblox.
    #[must_use]
    pub fn scopes(&self) -> &BTreeSet<String> {
        &self.scopes
    }

    /// Same-subject user information fetched with the new access token.
    #[must_use]
    pub fn user(&self) -> &RobloxUserInfo {
        &self.user
    }
}

impl fmt::Debug for RotatedRobloxTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotatedRobloxTokens")
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("id_token", &self.id_token)
            .field("expires_in_seconds", &self.expires_in_seconds)
            .field("scopes", &self.scopes)
            .field("user", &self.user)
            .finish()
    }
}

/// HTTP method used by the injectable OAuth transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthHttpMethod {
    /// Discovery, userinfo, or JWKS read.
    Get,
    /// Refresh-token form submission.
    PostForm,
}

#[derive(Clone)]
enum OAuthFormValue {
    Public(String),
    Secret(OAuthSecret),
}

/// Fully constructed provider request. Construction is private to this module.
#[derive(Clone)]
pub struct OAuthHttpRequest {
    method: OAuthHttpMethod,
    url: Url,
    bearer: Option<OAuthSecret>,
    form: BTreeMap<String, OAuthFormValue>,
}

impl OAuthHttpRequest {
    /// Request method.
    #[must_use]
    pub fn method(&self) -> OAuthHttpMethod {
        self.method
    }

    /// Validated provider URL.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Bearer secret for the concrete transport.
    #[must_use]
    pub fn bearer(&self) -> Option<&OAuthSecret> {
        self.bearer.as_ref()
    }

    /// Iterate form fields, exposing whether each value is secret.
    pub fn form_fields(&self) -> impl Iterator<Item = (&str, &str, bool)> {
        self.form.iter().map(|(key, value)| match value {
            OAuthFormValue::Public(value) => (key.as_str(), value.as_str(), false),
            OAuthFormValue::Secret(value) => (key.as_str(), value.expose_secret(), true),
        })
    }
}

impl fmt::Debug for OAuthHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fields: BTreeMap<_, _> = self
            .form
            .iter()
            .map(|(key, value)| {
                let shown = match value {
                    OAuthFormValue::Public(value) => value.as_str(),
                    OAuthFormValue::Secret(_) => "[REDACTED]",
                };
                (key, shown)
            })
            .collect();
        formatter
            .debug_struct("OAuthHttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("bearer", &self.bearer.as_ref().map(|_| "[REDACTED]"))
            .field("form", &fields)
            .finish()
    }
}

/// Bounded provider response returned by a transport.
#[derive(Clone)]
pub struct OAuthHttpResponse {
    /// HTTP status code.
    status: u16,
    /// Raw bounded body.
    body: Vec<u8>,
}

impl OAuthHttpResponse {
    /// Construct a response for a custom transport or deterministic test.
    #[must_use]
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self { status, body }
    }

    /// HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Raw body. It may contain secrets and must not be logged.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for OAuthHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthHttpResponse")
            .field("status", &self.status)
            .field(
                "body",
                &format_args!("[REDACTED; {} bytes]", self.body.len()),
            )
            .finish()
    }
}

/// Fixed transport failure classes without arbitrary secret-bearing strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OAuthTransportFailure {
    /// Connection or TLS failure.
    #[error("network failure")]
    Network,
    /// Connect or request deadline elapsed.
    #[error("transport timeout")]
    Timeout,
    /// Response exceeded the hard body limit.
    #[error("response body too large")]
    BodyTooLarge,
    /// HTTP request could not be represented or consumed.
    #[error("transport protocol failure")]
    Protocol,
}

/// Injectable transport boundary for deterministic refresh validation tests.
#[async_trait]
pub trait RobloxOAuthTransport: Send + Sync {
    /// Execute one prevalidated provider request.
    async fn execute(
        &self,
        request: OAuthHttpRequest,
    ) -> std::result::Result<OAuthHttpResponse, OAuthTransportFailure>;
}

/// Production reqwest transport with HTTPS-only, no redirects, bounded timeouts, and bounded bodies.
#[derive(Clone, Debug)]
pub struct ReqwestRobloxOAuthTransport {
    client: reqwest::Client,
}

impl ReqwestRobloxOAuthTransport {
    /// Construct the fixed safe transport profile.
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| {
                RobloxOAuthError::new(
                    "ROBLOX_OAUTH_TRANSPORT_INIT_FAILED",
                    RobloxOAuthErrorKind::InvalidRequest,
                    "Could not initialize the fixed Roblox OAuth transport",
                    false,
                )
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl RobloxOAuthTransport for ReqwestRobloxOAuthTransport {
    async fn execute(
        &self,
        request: OAuthHttpRequest,
    ) -> std::result::Result<OAuthHttpResponse, OAuthTransportFailure> {
        validate_provider_url(&request.url).map_err(|_| OAuthTransportFailure::Protocol)?;
        let mut builder = match request.method {
            OAuthHttpMethod::Get => self.client.get(request.url.clone()),
            OAuthHttpMethod::PostForm => self.client.post(request.url.clone()),
        };
        builder = builder.header(reqwest::header::ACCEPT, "application/json");
        if let Some(bearer) = request.bearer.as_ref() {
            builder = builder.bearer_auth(bearer.expose_secret());
        }
        if request.method == OAuthHttpMethod::PostForm {
            let form: Vec<_> = request
                .form_fields()
                .map(|(key, value, _)| (key.to_owned(), value.to_owned()))
                .collect();
            builder = builder.form(&form);
        }
        let mut response = builder.send().await.map_err(|error| {
            if error.is_timeout() {
                OAuthTransportFailure::Timeout
            } else {
                OAuthTransportFailure::Network
            }
        })?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_OAUTH_RESPONSE_BYTES as u64)
        {
            return Err(OAuthTransportFailure::BodyTooLarge);
        }
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                OAuthTransportFailure::Timeout
            } else {
                OAuthTransportFailure::Protocol
            }
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
                return Err(OAuthTransportFailure::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(OAuthHttpResponse { status, body })
    }
}

/// Validated discovery endpoints pinned to the Roblox HTTPS origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedRobloxDiscovery {
    /// Refresh token endpoint.
    pub token_endpoint: Url,
    /// Userinfo endpoint.
    pub userinfo_endpoint: Url,
    /// JSON Web Key Set endpoint.
    pub jwks_uri: Url,
}

/// Reusable refresh orchestrator over an injectable transport.
#[derive(Clone, Debug)]
pub struct RobloxOAuthRefresher<T> {
    config: RobloxPublicClientConfig,
    transport: T,
}

impl<T> RobloxOAuthRefresher<T>
where
    T: RobloxOAuthTransport,
{
    /// Bind a public client configuration to a transport.
    #[must_use]
    pub fn new(config: RobloxPublicClientConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Perform exactly one rotating refresh and validate the same immutable account.
    pub async fn refresh(
        &self,
        context: RobloxRefreshContext,
        now_unix_seconds: u64,
    ) -> Result<RotatedRobloxTokens> {
        let discovery_url = Url::parse(ROBLOX_OAUTH_DISCOVERY_URL).map_err(|_| {
            RobloxOAuthError::new(
                "ROBLOX_OAUTH_DISCOVERY_INVALID",
                RobloxOAuthErrorKind::InvalidRequest,
                "Compiled Roblox discovery URL is invalid",
                false,
            )
        })?;
        let discovery_response = self
            .send(OAuthHttpRequest {
                method: OAuthHttpMethod::Get,
                url: discovery_url,
                bearer: None,
                form: BTreeMap::new(),
            })
            .await?;
        require_success(&discovery_response, "ROBLOX_OAUTH_DISCOVERY_FAILED")?;
        let discovery = validate_discovery_document(&discovery_response.body)?;

        let mut form = BTreeMap::new();
        form.insert(
            "grant_type".to_owned(),
            OAuthFormValue::Public("refresh_token".to_owned()),
        );
        form.insert(
            "client_id".to_owned(),
            OAuthFormValue::Public(self.config.client_id.clone()),
        );
        form.insert(
            "refresh_token".to_owned(),
            OAuthFormValue::Secret(context.refresh_token.clone()),
        );
        debug_assert!(!form.contains_key("client_secret"));
        let token_response = self
            .send(OAuthHttpRequest {
                method: OAuthHttpMethod::PostForm,
                url: discovery.token_endpoint.clone(),
                bearer: None,
                form,
            })
            .await?;
        if !http_success(token_response.status) {
            return Err(classify_token_error(
                token_response.status,
                &token_response.body,
            ));
        }
        let tokens = validate_rotating_token_response(
            &token_response.body,
            &context.refresh_token,
            &self.config.required_scopes,
            self.config.minimum_access_lifetime_seconds,
        )?;

        let user_response = self
            .send(OAuthHttpRequest {
                method: OAuthHttpMethod::Get,
                url: discovery.userinfo_endpoint.clone(),
                bearer: Some(tokens.access_token.clone()),
                form: BTreeMap::new(),
            })
            .await?;
        require_success(&user_response, "ROBLOX_OAUTH_USERINFO_FAILED")?;
        let user = validate_userinfo_response(&user_response.body, &context.expected_subject)?;

        if let Some(id_token) = tokens.id_token.as_ref() {
            let jwks_response = self
                .send(OAuthHttpRequest {
                    method: OAuthHttpMethod::Get,
                    url: discovery.jwks_uri,
                    bearer: None,
                    form: BTreeMap::new(),
                })
                .await?;
            require_success(&jwks_response, "ROBLOX_OAUTH_JWKS_FAILED")?;
            verify_refresh_id_token(
                id_token.expose_secret(),
                &jwks_response.body,
                &self.config.client_id,
                &context.expected_subject,
                now_unix_seconds,
            )?;
        }

        Ok(RotatedRobloxTokens {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            id_token: tokens.id_token,
            expires_in_seconds: tokens.expires_in_seconds,
            scopes: tokens.scopes,
            user,
        })
    }

    async fn send(&self, request: OAuthHttpRequest) -> Result<OAuthHttpResponse> {
        validate_provider_url(&request.url)?;
        let response = self.transport.execute(request).await.map_err(|failure| {
            let code = match failure {
                OAuthTransportFailure::Timeout => "ROBLOX_OAUTH_TRANSPORT_TIMEOUT",
                OAuthTransportFailure::BodyTooLarge => "ROBLOX_OAUTH_BODY_TOO_LARGE",
                OAuthTransportFailure::Network | OAuthTransportFailure::Protocol => {
                    "ROBLOX_OAUTH_TRANSPORT_FAILED"
                }
            };
            RobloxOAuthError::new(
                code,
                RobloxOAuthErrorKind::ProviderUnavailable,
                "Roblox OAuth transport failed",
                !matches!(failure, OAuthTransportFailure::BodyTooLarge),
            )
        })?;
        if response.body.len() > MAX_OAUTH_RESPONSE_BYTES {
            return Err(RobloxOAuthError::new(
                "ROBLOX_OAUTH_BODY_TOO_LARGE",
                RobloxOAuthErrorKind::InvalidProviderResponse,
                "Roblox OAuth response exceeded the body limit",
                false,
            ));
        }
        Ok(response)
    }
}

#[derive(Debug, Deserialize)]
struct DiscoveryWire {
    issuer: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    jwks_uri: String,
}

/// Pure discovery validation with issuer and endpoint-origin pinning.
pub fn validate_discovery_document(body: &[u8]) -> Result<ValidatedRobloxDiscovery> {
    require_bounded(body)?;
    let discovery: DiscoveryWire = serde_json::from_slice(body).map_err(|_| {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_DISCOVERY_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox discovery document was invalid",
            false,
        )
    })?;
    if discovery.issuer != ROBLOX_OAUTH_ISSUER {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_ISSUER_MISMATCH",
            RobloxOAuthErrorKind::SecurityViolation,
            "Roblox discovery issuer did not match the pinned issuer",
            false,
        ));
    }
    let token_endpoint = parse_provider_endpoint(&discovery.token_endpoint)?;
    let userinfo_endpoint = parse_provider_endpoint(&discovery.userinfo_endpoint)?;
    let jwks_uri = parse_provider_endpoint(&discovery.jwks_uri)?;
    Ok(ValidatedRobloxDiscovery {
        token_endpoint,
        userinfo_endpoint,
        jwks_uri,
    })
}

#[derive(Deserialize)]
struct TokenWire {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: String,
}

#[derive(Debug)]
struct ValidatedTokenResponse {
    access_token: OAuthSecret,
    refresh_token: OAuthSecret,
    id_token: Option<OAuthSecret>,
    expires_in_seconds: u64,
    scopes: BTreeSet<String>,
}

/// Pure bearer, lifetime, scope, and mandatory rotation validation.
fn validate_rotating_token_response(
    body: &[u8],
    previous_refresh_token: &OAuthSecret,
    required_scopes: &BTreeSet<String>,
    minimum_access_lifetime_seconds: u64,
) -> Result<ValidatedTokenResponse> {
    require_bounded(body)?;
    let tokens: TokenWire = serde_json::from_slice(body).map_err(|_| {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_TOKEN_RESPONSE_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox refresh response was invalid",
            false,
        )
    })?;
    if !tokens.token_type.eq_ignore_ascii_case("bearer") {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_TOKEN_TYPE_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox refresh response did not contain a Bearer token",
            false,
        ));
    }
    if tokens.expires_in < minimum_access_lifetime_seconds {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_TOKEN_LIFETIME_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox access token lifetime was below the required minimum",
            false,
        ));
    }
    let scopes = parse_effective_scopes(&tokens.scope)?;
    if !required_scopes.is_subset(&scopes) {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_SCOPE_DOWNGRADE",
            RobloxOAuthErrorKind::SecurityViolation,
            "Roblox refresh response omitted a required scope",
            false,
        ));
    }
    let access_token = OAuthSecret::new(tokens.access_token).map_err(|_| {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_TOKEN_RESPONSE_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox access token was malformed",
            false,
        )
    })?;
    let refresh_token = OAuthSecret::new(tokens.refresh_token.ok_or_else(|| {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_REFRESH_ROTATION_MISSING",
            RobloxOAuthErrorKind::SecurityViolation,
            "Roblox refresh response omitted the required new refresh token",
            false,
        )
    })?)
    .map_err(|_| {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_REFRESH_ROTATION_MISSING",
            RobloxOAuthErrorKind::SecurityViolation,
            "Roblox refresh response contained an invalid new refresh token",
            false,
        )
    })?;
    if refresh_token == *previous_refresh_token {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_REFRESH_NOT_ROTATED",
            RobloxOAuthErrorKind::SecurityViolation,
            "Roblox returned the already-consumed refresh token instead of rotating it",
            false,
        ));
    }
    let id_token = tokens
        .id_token
        .map(OAuthSecret::new)
        .transpose()
        .map_err(|_| {
            RobloxOAuthError::new(
                "ROBLOX_OAUTH_ID_TOKEN_INVALID",
                RobloxOAuthErrorKind::InvalidProviderResponse,
                "Roblox ID token was malformed",
                false,
            )
        })?;
    Ok(ValidatedTokenResponse {
        access_token,
        refresh_token,
        id_token,
        expires_in_seconds: tokens.expires_in,
        scopes,
    })
}

#[derive(Debug, Deserialize)]
struct UserInfoWire {
    sub: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

/// Pure userinfo parsing and immutable-subject correlation.
pub fn validate_userinfo_response(body: &[u8], expected_subject: &str) -> Result<RobloxUserInfo> {
    require_bounded(body)?;
    validate_subject(expected_subject, RobloxOAuthErrorKind::InvalidRequest)?;
    let user: UserInfoWire = serde_json::from_slice(body).map_err(|_| {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_USERINFO_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox userinfo response was invalid",
            false,
        )
    })?;
    validate_subject(&user.sub, RobloxOAuthErrorKind::InvalidProviderResponse)?;
    if user.sub != expected_subject {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_SUBJECT_MISMATCH",
            RobloxOAuthErrorKind::SecurityViolation,
            "Roblox userinfo subject changed during refresh",
            false,
        ));
    }
    validate_optional_profile(&user.name)?;
    validate_optional_profile(&user.preferred_username)?;
    Ok(RobloxUserInfo {
        sub: user.sub,
        name: user.name,
        preferred_username: user.preferred_username,
    })
}

#[derive(Debug, Deserialize)]
struct JwkSetWire {
    keys: Vec<EcJwkWire>,
}

#[derive(Debug, Deserialize)]
struct EcJwkWire {
    kid: String,
    kty: String,
    crv: String,
    x: String,
    y: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(rename = "use", default)]
    usage: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RefreshIdClaims {
    sub: String,
    iss: String,
    aud: Value,
    exp: u64,
    iat: u64,
    #[serde(default)]
    nonce: Option<String>,
}

/// Verify an optional refresh ID token without requiring the authorization nonce.
///
/// Signature, ES256, key type, issuer, audience, expiry, issued-at tolerance, and
/// immutable subject are still mandatory.
pub fn verify_refresh_id_token(
    token: &str,
    jwks_body: &[u8],
    audience: &str,
    expected_subject: &str,
    now_unix_seconds: u64,
) -> Result<()> {
    if token.is_empty() || token.len() > MAX_SECRET_BYTES {
        return Err(id_token_error());
    }
    require_bounded(jwks_body)?;
    validate_subject(expected_subject, RobloxOAuthErrorKind::InvalidRequest)?;
    let header = decode_header(token).map_err(|_| id_token_error())?;
    if header.alg != Algorithm::ES256 {
        return Err(id_token_error());
    }
    let kid = header.kid.ok_or_else(id_token_error)?;
    let jwks: JwkSetWire = serde_json::from_slice(jwks_body).map_err(|_| id_token_error())?;
    if jwks.keys.is_empty() || jwks.keys.len() > MAX_JWK_COUNT {
        return Err(id_token_error());
    }
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .ok_or_else(id_token_error)?;
    if key.kty != "EC"
        || key.crv != "P-256"
        || key.alg.as_deref().is_some_and(|value| value != "ES256")
        || key.usage.as_deref().is_some_and(|value| value != "sig")
    {
        return Err(id_token_error());
    }
    let decoding_key =
        DecodingKey::from_ec_components(&key.x, &key.y).map_err(|_| id_token_error())?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.set_issuer(&[ROBLOX_OAUTH_ISSUER]);
    validation.set_audience(&[audience]);
    validation.sub = Some(expected_subject.to_owned());
    validation.validate_exp = false;
    validation.leeway = CLOCK_SKEW_SECONDS;
    let claims = decode::<RefreshIdClaims>(token, &decoding_key, &validation)
        .map_err(|_| id_token_error())?
        .claims;
    if claims.iss != ROBLOX_OAUTH_ISSUER
        || claims.sub != expected_subject
        || claims.exp <= now_unix_seconds
        || claims.exp <= claims.iat
        || claims.iat > now_unix_seconds.saturating_add(CLOCK_SKEW_SECONDS)
    {
        return Err(id_token_error());
    }
    let _audience_shape_validated_by_library = &claims.aud;
    let _authorization_nonce_is_not_reused_during_refresh = &claims.nonce;
    Ok(())
}

fn id_token_error() -> RobloxOAuthError {
    RobloxOAuthError::new(
        "ROBLOX_OAUTH_ID_TOKEN_INVALID",
        RobloxOAuthErrorKind::SecurityViolation,
        "Roblox refresh ID token signature or correlation claims were invalid",
        false,
    )
}

fn validate_required_scopes<I, S>(scopes: I) -> Result<BTreeSet<String>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let scopes: BTreeSet<String> = scopes.into_iter().map(Into::into).collect();
    if scopes.is_empty()
        || scopes.len() > MAX_SCOPE_COUNT
        || scopes.iter().any(|scope| !valid_scope(scope))
    {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_SCOPE_INVALID",
            RobloxOAuthErrorKind::InvalidRequest,
            "Required OAuth scope set was empty, oversized, or malformed",
            false,
        ));
    }
    Ok(scopes)
}

fn parse_effective_scopes(value: &str) -> Result<BTreeSet<String>> {
    let split: Vec<_> = value.split_whitespace().collect();
    if split.is_empty()
        || split.len() > MAX_SCOPE_COUNT
        || split.iter().any(|scope| !valid_scope(scope))
    {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_SCOPE_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox effective scope response was invalid",
            false,
        ));
    }
    Ok(split.into_iter().map(str::to_owned).collect())
}

fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 256
        && scope.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-' | b'.' | b'/')
        })
}

fn validate_subject(subject: &str, kind: RobloxOAuthErrorKind) -> Result<()> {
    if subject.is_empty()
        || subject.len() > MAX_SUBJECT_BYTES
        || subject.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_SUBJECT_INVALID",
            kind,
            "Roblox immutable subject was malformed",
            false,
        ));
    }
    Ok(())
}

fn validate_optional_profile(value: &Option<String>) -> Result<()> {
    if value.as_ref().is_some_and(|value| {
        value.len() > 1024
            || value
                .bytes()
                .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
    }) {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_USERINFO_INVALID",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox user profile field was malformed",
            false,
        ));
    }
    Ok(())
}

fn parse_provider_endpoint(value: &str) -> Result<Url> {
    let parsed = Url::parse(value).map_err(|_| origin_error())?;
    validate_provider_url(&parsed)?;
    Ok(parsed)
}

fn validate_provider_url(url: &Url) -> Result<()> {
    if url.scheme() != "https"
        || url.host_str() != Some("apis.roblox.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.origin().ascii_serialization() != ROBLOX_OAUTH_ORIGIN
    {
        return Err(origin_error());
    }
    Ok(())
}

fn origin_error() -> RobloxOAuthError {
    RobloxOAuthError::new(
        "ROBLOX_OAUTH_ORIGIN_REJECTED",
        RobloxOAuthErrorKind::SecurityViolation,
        "Roblox OAuth endpoint escaped the pinned HTTPS origin",
        false,
    )
}

fn require_bounded(body: &[u8]) -> Result<()> {
    if body.len() > MAX_OAUTH_RESPONSE_BYTES {
        return Err(RobloxOAuthError::new(
            "ROBLOX_OAUTH_BODY_TOO_LARGE",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox OAuth response exceeded the body limit",
            false,
        ));
    }
    Ok(())
}

fn http_success(status: u16) -> bool {
    (200..300).contains(&status)
}

fn require_success(response: &OAuthHttpResponse, code: &'static str) -> Result<()> {
    if http_success(response.status) {
        Ok(())
    } else {
        Err(RobloxOAuthError::new(
            code,
            if response.status >= 500 {
                RobloxOAuthErrorKind::ProviderUnavailable
            } else {
                RobloxOAuthErrorKind::InvalidProviderResponse
            },
            "Roblox OAuth endpoint returned an unsuccessful status",
            response.status >= 500,
        ))
    }
}

#[derive(Deserialize)]
struct OAuthErrorWire {
    error: String,
}

fn classify_token_error(status: u16, body: &[u8]) -> RobloxOAuthError {
    let error = if body.len() <= MAX_OAUTH_RESPONSE_BYTES {
        serde_json::from_slice::<OAuthErrorWire>(body)
            .ok()
            .map(|wire| wire.error)
    } else {
        None
    };
    if error.as_deref() == Some("invalid_grant") {
        RobloxOAuthError::reauthentication_required()
    } else if status >= 500 || error.as_deref() == Some("temporarily_unavailable") {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_PROVIDER_UNAVAILABLE",
            RobloxOAuthErrorKind::ProviderUnavailable,
            "Roblox OAuth token endpoint is temporarily unavailable",
            true,
        )
    } else {
        RobloxOAuthError::new(
            "ROBLOX_OAUTH_REFRESH_REJECTED",
            RobloxOAuthErrorKind::InvalidProviderResponse,
            "Roblox rejected the public-client refresh request",
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWTFfCGljY6aw3Hrt
kHmPRiazukxPLb6ilpRAewjW8nihRANCAATDskChT+Altkm9X7MI69T3IUmrQU0L
950IxEzvw/x5BMEINRMrXLBJhqzO9Bm+d6JbqA21YQmd1Kt4RzLJR1W+
-----END PRIVATE KEY-----"#;
    const TEST_JWKS: &str = r#"{"keys":[{"kty":"EC","crv":"P-256","x":"w7JAoU_gJbZJvV-zCOvU9yFJq0FNC_edCMRM78P8eQQ","y":"wQg1EytcsEmGrM70Gb53oluoDbVhCZ3Uq3hHMslHVb4","kid":"ec01","alg":"ES256","use":"sig"}]}"#;

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn discovery(origin: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "issuer": ROBLOX_OAUTH_ISSUER,
            "token_endpoint": format!("{origin}/oauth/v1/token"),
            "userinfo_endpoint": format!("{origin}/oauth/v1/userinfo"),
            "jwks_uri": format!("{origin}/oauth/v1/certs")
        }))
        .unwrap()
    }

    fn token_body(refresh: &str, id_token: Option<&str>) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "access_token": "new-access-secret",
            "refresh_token": refresh,
            "id_token": id_token,
            "token_type": "Bearer",
            "expires_in": 900,
            "scope": "openid profile universe:read"
        }))
        .unwrap()
    }

    fn signed_id_token(
        subject: &str,
        audience: &str,
        nonce: Option<&str>,
        timestamp: u64,
    ) -> String {
        let claims = RefreshIdClaims {
            sub: subject.to_owned(),
            iss: ROBLOX_OAUTH_ISSUER.to_owned(),
            aud: Value::String(audience.to_owned()),
            exp: timestamp + 600,
            iat: timestamp,
            nonce: nonce.map(str::to_owned),
        };
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("ec01".to_owned());
        encode(
            &header,
            &claims,
            &EncodingKey::from_ec_pem(TEST_PRIVATE_KEY.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    #[derive(Default)]
    struct FakeTransport {
        responses: Mutex<VecDeque<std::result::Result<OAuthHttpResponse, OAuthTransportFailure>>>,
        requests: Mutex<Vec<OAuthHttpRequest>>,
    }

    impl FakeTransport {
        fn with(responses: Vec<OAuthHttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RobloxOAuthTransport for FakeTransport {
        async fn execute(
            &self,
            request: OAuthHttpRequest,
        ) -> std::result::Result<OAuthHttpResponse, OAuthTransportFailure> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("fake response")
        }
    }

    #[test]
    fn discovery_pins_issuer_and_https_origin() {
        let valid = validate_discovery_document(&discovery(ROBLOX_OAUTH_ORIGIN)).unwrap();
        assert_eq!(valid.token_endpoint.host_str(), Some("apis.roblox.com"));
        assert_eq!(
            validate_discovery_document(&discovery("https://evil.example"))
                .unwrap_err()
                .code,
            "ROBLOX_OAUTH_ORIGIN_REJECTED"
        );
        let wrong_issuer = serde_json::json!({
            "issuer": "https://evil.example/",
            "token_endpoint": "https://apis.roblox.com/oauth/v1/token",
            "userinfo_endpoint": "https://apis.roblox.com/oauth/v1/userinfo",
            "jwks_uri": "https://apis.roblox.com/oauth/v1/certs"
        });
        assert_eq!(
            validate_discovery_document(&serde_json::to_vec(&wrong_issuer).unwrap())
                .unwrap_err()
                .code,
            "ROBLOX_OAUTH_ISSUER_MISMATCH"
        );
    }

    #[test]
    fn pure_token_validation_requires_rotation_bearer_lifetime_and_scopes() {
        let old = OAuthSecret::new("old-refresh-secret").unwrap();
        let scopes = RobloxPublicClientConfig::recommended("public-client")
            .unwrap()
            .required_scopes
            .clone();
        let valid = validate_rotating_token_response(
            &token_body("new-refresh-secret", None),
            &old,
            &scopes,
            60,
        )
        .unwrap();
        assert_eq!(valid.refresh_token.expose_secret(), "new-refresh-secret");

        assert_eq!(
            validate_rotating_token_response(
                &token_body("old-refresh-secret", None),
                &old,
                &scopes,
                60
            )
            .unwrap_err()
            .code,
            "ROBLOX_OAUTH_REFRESH_NOT_ROTATED"
        );
        let missing_rotation = serde_json::json!({
            "access_token": "access", "token_type": "Bearer", "expires_in": 900,
            "scope": "openid profile universe:read"
        });
        assert_eq!(
            validate_rotating_token_response(
                &serde_json::to_vec(&missing_rotation).unwrap(),
                &old,
                &scopes,
                60
            )
            .unwrap_err()
            .code,
            "ROBLOX_OAUTH_REFRESH_ROTATION_MISSING"
        );
        let downgrade = serde_json::json!({
            "access_token": "access", "refresh_token": "new", "token_type": "bearer",
            "expires_in": 30, "scope": "openid"
        });
        assert_eq!(
            validate_rotating_token_response(
                &serde_json::to_vec(&downgrade).unwrap(),
                &old,
                &scopes,
                60
            )
            .unwrap_err()
            .code,
            "ROBLOX_OAUTH_TOKEN_LIFETIME_INVALID"
        );
    }

    #[test]
    fn id_token_verifies_signature_subject_and_does_not_require_nonce() {
        let timestamp = now();
        let token = signed_id_token("roblox-subject", "public-client", None, timestamp);
        verify_refresh_id_token(
            &token,
            TEST_JWKS.as_bytes(),
            "public-client",
            "roblox-subject",
            timestamp,
        )
        .unwrap();
        let token_with_old_nonce = signed_id_token(
            "roblox-subject",
            "public-client",
            Some("authorization-nonce"),
            timestamp,
        );
        verify_refresh_id_token(
            &token_with_old_nonce,
            TEST_JWKS.as_bytes(),
            "public-client",
            "roblox-subject",
            timestamp,
        )
        .unwrap();
        assert!(
            verify_refresh_id_token(
                &token,
                TEST_JWKS.as_bytes(),
                "public-client",
                "different-subject",
                timestamp
            )
            .is_err()
        );
        let mut tampered = token.into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'a' { b'b' } else { b'a' };
        assert!(
            verify_refresh_id_token(
                std::str::from_utf8(&tampered).unwrap(),
                TEST_JWKS.as_bytes(),
                "public-client",
                "roblox-subject",
                timestamp
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn end_to_end_refresh_correlates_subject_and_omits_client_secret() {
        let transport = FakeTransport::with(vec![
            OAuthHttpResponse {
                status: 200,
                body: discovery(ROBLOX_OAUTH_ORIGIN),
            },
            OAuthHttpResponse {
                status: 200,
                body: token_body("rotated-refresh-secret", None),
            },
            OAuthHttpResponse {
                status: 200,
                body: br#"{"sub":"same-subject","preferred_username":"Builder"}"#.to_vec(),
            },
        ]);
        let refresher = RobloxOAuthRefresher::new(
            RobloxPublicClientConfig::recommended("public-client").unwrap(),
            transport,
        );
        let context = RobloxRefreshContext::new(
            OAuthSecret::new("old-refresh-secret").unwrap(),
            "same-subject",
        )
        .unwrap();
        let rotated = refresher.refresh(context, now()).await.unwrap();
        assert_eq!(rotated.user().sub, "same-subject");
        assert_eq!(
            rotated.refresh_token().expose_secret(),
            "rotated-refresh-secret"
        );
        let requests = refresher.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let refresh = &requests[1];
        assert_eq!(refresh.method(), OAuthHttpMethod::PostForm);
        assert!(!refresh.form.contains_key("client_secret"));
        assert!(format!("{refresh:?}").contains("[REDACTED]"));
        assert!(!format!("{refresh:?}").contains("old-refresh-secret"));
    }

    #[tokio::test]
    async fn subject_change_and_invalid_grant_fail_closed() {
        let mismatch = FakeTransport::with(vec![
            OAuthHttpResponse {
                status: 200,
                body: discovery(ROBLOX_OAUTH_ORIGIN),
            },
            OAuthHttpResponse {
                status: 200,
                body: token_body("rotated", None),
            },
            OAuthHttpResponse {
                status: 200,
                body: br#"{"sub":"other-subject"}"#.to_vec(),
            },
        ]);
        let config = RobloxPublicClientConfig::recommended("public-client").unwrap();
        let context = RobloxRefreshContext::new(
            OAuthSecret::new("old-refresh-secret").unwrap(),
            "same-subject",
        )
        .unwrap();
        let error = RobloxOAuthRefresher::new(config.clone(), mismatch)
            .refresh(context, now())
            .await
            .unwrap_err();
        assert_eq!(error.code, "ROBLOX_OAUTH_SUBJECT_MISMATCH");

        let invalid_grant = FakeTransport::with(vec![
            OAuthHttpResponse {
                status: 200,
                body: discovery(ROBLOX_OAUTH_ORIGIN),
            },
            OAuthHttpResponse {
                status: 400,
                body: br#"{"error":"invalid_grant","error_description":"old-refresh-secret"}"#
                    .to_vec(),
            },
        ]);
        let invalid_context = RobloxRefreshContext::new(
            OAuthSecret::new("old-refresh-secret").unwrap(),
            "same-subject",
        )
        .unwrap();
        let error = RobloxOAuthRefresher::new(config, invalid_grant)
            .refresh(invalid_context, now())
            .await
            .unwrap_err();
        assert_eq!(error.kind, RobloxOAuthErrorKind::ReauthenticationRequired);
        assert!(!error.retryable);
        assert!(!format!("{error:?}").contains("old-refresh-secret"));
        assert!(!error.to_string().contains("old-refresh-secret"));
    }

    #[tokio::test]
    async fn optional_id_token_is_checked_after_same_subject_userinfo() {
        let timestamp = now();
        let id_token = signed_id_token("same-subject", "public-client", None, timestamp);
        let transport = FakeTransport::with(vec![
            OAuthHttpResponse {
                status: 200,
                body: discovery(ROBLOX_OAUTH_ORIGIN),
            },
            OAuthHttpResponse {
                status: 200,
                body: token_body("rotated-refresh-secret", Some(&id_token)),
            },
            OAuthHttpResponse {
                status: 200,
                body: br#"{"sub":"same-subject"}"#.to_vec(),
            },
            OAuthHttpResponse {
                status: 200,
                body: TEST_JWKS.as_bytes().to_vec(),
            },
        ]);
        let refresher = RobloxOAuthRefresher::new(
            RobloxPublicClientConfig::recommended("public-client").unwrap(),
            transport,
        );
        let context = RobloxRefreshContext::new(
            OAuthSecret::new("old-refresh-secret").unwrap(),
            "same-subject",
        )
        .unwrap();
        let rotated = refresher.refresh(context, timestamp).await.unwrap();
        assert!(rotated.id_token().is_some());
        assert_eq!(refresher.transport.requests.lock().unwrap().len(), 4);
    }
}
