//! Read-only Roblox OAuth resource discovery and provider-evidence boundary.
//!
//! This module intentionally contains no persistence and no mutation endpoint. The desktop owns
//! the operating-system vault boundary and passes an in-memory access token to this provider. The
//! token is redacted from formatting and is never part of a serializable public result.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::roblox_oauth::OAuthSecret;

/// Exact Roblox HTTPS origin used by OAuth and Open Cloud resource reads.
pub const ROBLOX_API_ORIGIN: &str = "https://apis.roblox.com";
/// Exact public-client token-resource endpoint used to enumerate grants.
pub const ROBLOX_TOKEN_RESOURCES_ENDPOINT: &str =
    "https://apis.roblox.com/oauth/v1/token/resources";
/// Exact documented legacy origin used only to discover candidate place IDs.
pub const ROBLOX_DEVELOP_ORIGIN: &str = "https://develop.roblox.com";
/// Required OAuth scope for the read-only resource inventory.
pub const ROBLOX_RESOURCE_SCOPE: &str = "universe:read";

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_AUTHORIZATION_RECORDS: usize = 100;
const MAX_AUTHORIZED_UNIVERSES: usize = 50;
const MAX_PLACES_PER_UNIVERSE: usize = 500;
const MAX_PLACE_INDEX_PAGES: usize = 5;

/// Error category safe to expose to the desktop renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RobloxResourceErrorKind {
    /// The caller supplied an invalid identifier or public-client value.
    InvalidRequest,
    /// The credential is absent, expired, or rejected by Roblox.
    ReauthenticationRequired,
    /// The token does not grant the requested Roblox resource.
    AccessDenied,
    /// Roblox returned an invalid or security-sensitive response.
    InvalidProviderResponse,
    /// Roblox is unavailable or throttling the caller.
    ProviderUnavailable,
}

/// Redacted typed failure from the resource provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxResourceError {
    /// Stable machine-readable error code.
    pub code: &'static str,
    /// Broad safe error category.
    pub kind: RobloxResourceErrorKind,
    /// Non-secret user-facing explanation.
    pub message: &'static str,
    /// Whether a later read-only retry may succeed without user action.
    pub retryable: bool,
}

impl fmt::Display for RobloxResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RobloxResourceError {}

/// Result returned by the Roblox resource provider.
pub type Result<T> = std::result::Result<T, RobloxResourceError>;

impl RobloxResourceError {
    const fn new(
        code: &'static str,
        kind: RobloxResourceErrorKind,
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
}

/// HTTP method allowed by the fixed resource provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceHttpMethod {
    /// Read an exact provider resource.
    Get,
    /// Submit the fixed OAuth token-resource form.
    PostForm,
}

#[derive(Clone)]
enum ResourceFormValue {
    Public(String),
    Secret(OAuthSecret),
}

/// Fixed provider HTTP request with redacted secret formatting.
#[derive(Clone)]
pub struct ResourceHttpRequest {
    method: ResourceHttpMethod,
    url: Url,
    bearer: Option<OAuthSecret>,
    form: Vec<(String, ResourceFormValue)>,
}

impl ResourceHttpRequest {
    /// Return the fixed HTTP method.
    #[must_use]
    pub const fn method(&self) -> ResourceHttpMethod {
        self.method
    }

    /// Return the validated destination URL.
    #[must_use]
    pub const fn url(&self) -> &Url {
        &self.url
    }

    fn bearer(&self) -> Option<&OAuthSecret> {
        self.bearer.as_ref()
    }
}

impl fmt::Debug for ResourceHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let form = self
            .form
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str(),
                    match value {
                        ResourceFormValue::Public(value) => value.as_str(),
                        ResourceFormValue::Secret(_) => "[REDACTED]",
                    },
                )
            })
            .collect::<Vec<_>>();
        formatter
            .debug_struct("ResourceHttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("bearer", &self.bearer.as_ref().map(|_| "[REDACTED]"))
            .field("form", &form)
            .finish()
    }
}

/// Bounded response produced by a resource transport.
#[derive(Clone)]
pub struct ResourceHttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body, already bounded by the transport.
    pub body: Vec<u8>,
}

impl fmt::Debug for ResourceHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceHttpResponse")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Non-secret failure raised by an HTTP transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceTransportFailure {
    /// The request timed out.
    Timeout,
    /// The network request failed.
    Network,
    /// The response exceeded the fixed byte ceiling.
    BodyTooLarge,
    /// The request or response violated the fixed protocol.
    Protocol,
}

/// Injectable transport used for deterministic provider tests.
#[async_trait]
pub trait RobloxResourceTransport: Send + Sync {
    /// Send one fixed provider request.
    async fn send(
        &self,
        request: ResourceHttpRequest,
    ) -> std::result::Result<ResourceHttpResponse, ResourceTransportFailure>;
}

/// Production HTTPS transport with no redirects and strict response bounds.
#[derive(Clone)]
pub struct ReqwestRobloxResourceTransport {
    client: reqwest::Client,
}

impl ReqwestRobloxResourceTransport {
    /// Construct the pinned, no-redirect Roblox resource transport.
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(25))
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                RobloxResourceError::new(
                    "ROBLOX_RESOURCE_TRANSPORT_INIT",
                    RobloxResourceErrorKind::InvalidRequest,
                    "Could not initialize the fixed Roblox resource transport",
                    false,
                )
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl RobloxResourceTransport for ReqwestRobloxResourceTransport {
    async fn send(
        &self,
        request: ResourceHttpRequest,
    ) -> std::result::Result<ResourceHttpResponse, ResourceTransportFailure> {
        validate_destination(request.url()).map_err(|_| ResourceTransportFailure::Protocol)?;
        let mut builder = match request.method {
            ResourceHttpMethod::Get => self.client.get(request.url.clone()),
            ResourceHttpMethod::PostForm => self.client.post(request.url.clone()),
        }
        .header(reqwest::header::ACCEPT, "application/json");
        if let Some(bearer) = request.bearer() {
            builder = builder.bearer_auth(bearer.expose_secret());
        }
        if request.method == ResourceHttpMethod::PostForm {
            let form = request
                .form
                .iter()
                .map(|(key, value)| {
                    let value = match value {
                        ResourceFormValue::Public(value) => value.as_str(),
                        ResourceFormValue::Secret(value) => value.expose_secret(),
                    };
                    (key.as_str(), value)
                })
                .collect::<Vec<_>>();
            builder = builder.form(&form);
        }
        let response = builder.send().await.map_err(|error| {
            if error.is_timeout() {
                ResourceTransportFailure::Timeout
            } else {
                ResourceTransportFailure::Network
            }
        })?;
        if response.url() != request.url() {
            return Err(ResourceTransportFailure::Protocol);
        }
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ResourceTransportFailure::BodyTooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                if error.is_timeout() {
                    ResourceTransportFailure::Timeout
                } else {
                    ResourceTransportFailure::Protocol
                }
            })?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ResourceTransportFailure::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(ResourceHttpResponse { status, body })
    }
}

/// Roblox owner attached to an OAuth target grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxResourceOwner {
    /// Roblox numeric owner ID.
    pub id: String,
    /// Roblox owner type (`User` or `Group`).
    pub owner_type: String,
}

/// Read-only place metadata shown by the desktop.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxPlaceMetadata {
    /// Numeric Roblox place ID.
    pub id: String,
    /// Numeric parent universe ID.
    pub universe_id: String,
    /// Canonical Open Cloud resource path.
    pub path: String,
    /// Provider display name.
    pub display_name: String,
    /// Provider description, when returned.
    pub description: Option<String>,
    /// Whether this is the universe root place.
    pub root: bool,
    /// Maximum server size, when returned by the exact Open Cloud read.
    pub server_size: Option<u32>,
    /// Provider creation timestamp, when returned.
    pub create_time: Option<String>,
    /// Provider update timestamp, when returned.
    pub update_time: Option<String>,
    /// True only after the exact OAuth Get Place response was validated.
    pub oauth_validated: bool,
}

/// Read-only universe metadata shown by the desktop.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxUniverseMetadata {
    /// Numeric Roblox universe ID.
    pub id: String,
    /// Canonical Open Cloud resource path.
    pub path: String,
    /// Provider display name.
    pub display_name: String,
    /// Provider description, when returned.
    pub description: Option<String>,
    /// Provider visibility enum, when returned.
    pub visibility: Option<String>,
    /// Canonical creator resource path (`users/{id}` or `groups/{id}`).
    pub creator: Option<String>,
    /// Numeric root place ID.
    pub root_place_id: String,
    /// Provider creation timestamp, when returned.
    pub create_time: Option<String>,
    /// Provider update timestamp, when returned.
    pub update_time: Option<String>,
    /// Candidate places from the documented read-only place index.
    pub places: Vec<RobloxPlaceMetadata>,
}

/// Public read-only capability declaration returned with every inventory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxResourceCapabilities {
    /// OAuth scope required by this boundary.
    pub required_scope: &'static str,
    /// Whether desktop metadata mutation is exposed.
    pub desktop_mutation: bool,
    /// Whether desktop place publication is exposed.
    pub desktop_place_publishing: bool,
    /// Credential profile required for place publication.
    pub place_publishing_credential: &'static str,
}

impl Default for RobloxResourceCapabilities {
    fn default() -> Self {
        Self {
            required_scope: ROBLOX_RESOURCE_SCOPE,
            desktop_mutation: false,
            desktop_place_publishing: false,
            place_publishing_credential: "operator-api-key-only",
        }
    }
}

/// Authorized resource inventory derived from Roblox provider reads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxResourceInventory {
    /// OAuth target owners returned by Roblox.
    pub owners: Vec<RobloxResourceOwner>,
    /// Explicitly enumerable, OAuth-validated universes.
    pub universes: Vec<RobloxUniverseMetadata>,
    /// Whether Roblox returned an owner-wide `U` grant that cannot itself enumerate IDs.
    pub has_owner_wide_grant: bool,
    /// Non-secret limitations encountered while building the inventory.
    pub warnings: Vec<String>,
    /// Hard read-only capability declaration.
    pub capabilities: RobloxResourceCapabilities,
}

/// Fresh provider evidence for an exact selected universe/place pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxSelectionEvidence {
    /// Exact OAuth-validated universe metadata.
    pub universe: RobloxUniverseMetadata,
    /// Exact OAuth-validated place metadata.
    pub place: RobloxPlaceMetadata,
    /// Local observation time supplied by the native caller.
    pub observed_at_unix_seconds: u64,
    /// Fixed evidence source identifier.
    pub source: &'static str,
    /// Credential mode required for publication; never the desktop OAuth token.
    pub place_publishing_credential: &'static str,
}

/// Read-only Roblox provider over an injectable transport.
pub struct RobloxResourceProvider<T> {
    transport: T,
}

impl<T> RobloxResourceProvider<T>
where
    T: RobloxResourceTransport,
{
    /// Construct a read-only provider.
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Enumerate explicit universe grants and their candidate places.
    pub async fn discover(
        &self,
        access_token: &OAuthSecret,
        client_id: &str,
    ) -> Result<RobloxResourceInventory> {
        validate_client_id(client_id)?;
        let grants = self.read_grants(access_token, client_id).await?;
        if grants.explicit_universe_ids.len() > MAX_AUTHORIZED_UNIVERSES {
            return Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_UNIVERSE_LIMIT",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned more explicit universe grants than this client can safely enumerate",
                false,
            ));
        }
        let mut universes = Vec::with_capacity(grants.explicit_universe_ids.len());
        let mut warnings = Vec::new();
        for universe_id in &grants.explicit_universe_ids {
            universes.push(
                self.read_universe_with_places(access_token, universe_id, &mut warnings)
                    .await?,
            );
        }
        if grants.has_owner_wide_grant {
            warnings.push(
                "Roblox returned an owner-wide target grant without concrete universe IDs; enter an exact universe ID to validate it."
                    .to_owned(),
            );
        }
        Ok(RobloxResourceInventory {
            owners: grants.owners,
            universes,
            has_owner_wide_grant: grants.has_owner_wide_grant,
            warnings,
            capabilities: RobloxResourceCapabilities::default(),
        })
    }

    /// Validate and read an exact universe, including owner-wide grant cases.
    pub async fn probe_universe(
        &self,
        access_token: &OAuthSecret,
        client_id: &str,
        universe_id: &str,
    ) -> Result<RobloxUniverseMetadata> {
        validate_client_id(client_id)?;
        let universe_id = validate_numeric_id(universe_id)?;
        let grants = self.read_grants(access_token, client_id).await?;
        authorize_universe(&grants, &universe_id)?;
        self.read_universe_with_places(access_token, &universe_id, &mut Vec::new())
            .await
    }

    /// Produce fresh typed evidence for an exact universe/place selection.
    pub async fn read_selection(
        &self,
        access_token: &OAuthSecret,
        client_id: &str,
        universe_id: &str,
        place_id: &str,
        observed_at_unix_seconds: u64,
    ) -> Result<RobloxSelectionEvidence> {
        validate_client_id(client_id)?;
        let universe_id = validate_numeric_id(universe_id)?;
        let place_id = validate_numeric_id(place_id)?;
        let grants = self.read_grants(access_token, client_id).await?;
        authorize_universe(&grants, &universe_id)?;
        let mut universe = self.read_universe(access_token, &universe_id).await?;
        let place = self
            .read_place(access_token, &universe_id, &place_id)
            .await?;
        if place.universe_id != universe_id {
            return Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_PLACE_PARENT_MISMATCH",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned a place outside the selected universe",
                false,
            ));
        }
        universe.places = vec![place.clone()];
        Ok(RobloxSelectionEvidence {
            universe,
            place,
            observed_at_unix_seconds,
            source: "roblox-open-cloud-oauth-read-v1",
            place_publishing_credential: "operator-api-key-only",
        })
    }

    async fn read_grants(
        &self,
        access_token: &OAuthSecret,
        client_id: &str,
    ) -> Result<AuthorizationSnapshot> {
        let request = ResourceHttpRequest {
            method: ResourceHttpMethod::PostForm,
            url: Url::parse(ROBLOX_TOKEN_RESOURCES_ENDPOINT).expect("fixed Roblox URL is valid"),
            bearer: None,
            form: vec![
                (
                    "token".to_owned(),
                    ResourceFormValue::Secret(access_token.clone()),
                ),
                (
                    "client_id".to_owned(),
                    ResourceFormValue::Public(client_id.to_owned()),
                ),
            ],
        };
        let wire: TokenResourcesWire = self.send_json(request).await?;
        parse_authorization_snapshot(wire)
    }

    async fn read_universe(
        &self,
        access_token: &OAuthSecret,
        universe_id: &str,
    ) -> Result<RobloxUniverseMetadata> {
        let url = fixed_api_url(&format!("/cloud/v2/universes/{universe_id}"))?;
        let wire: UniverseWire = self
            .send_json(ResourceHttpRequest {
                method: ResourceHttpMethod::Get,
                url,
                bearer: Some(access_token.clone()),
                form: Vec::new(),
            })
            .await?;
        universe_from_wire(universe_id, wire)
    }

    async fn read_place(
        &self,
        access_token: &OAuthSecret,
        universe_id: &str,
        place_id: &str,
    ) -> Result<RobloxPlaceMetadata> {
        let url = fixed_api_url(&format!(
            "/cloud/v2/universes/{universe_id}/places/{place_id}"
        ))?;
        let wire: PlaceWire = self
            .send_json(ResourceHttpRequest {
                method: ResourceHttpMethod::Get,
                url,
                bearer: Some(access_token.clone()),
                form: Vec::new(),
            })
            .await?;
        place_from_wire(universe_id, place_id, wire)
    }

    async fn read_universe_with_places(
        &self,
        access_token: &OAuthSecret,
        universe_id: &str,
        warnings: &mut Vec<String>,
    ) -> Result<RobloxUniverseMetadata> {
        let mut universe = self.read_universe(access_token, universe_id).await?;
        match self.read_place_index(universe_id).await {
            Ok(mut places) => {
                for place in &mut places {
                    place.root = place.id == universe.root_place_id;
                }
                if !places
                    .iter()
                    .any(|place| place.id == universe.root_place_id)
                {
                    match self
                        .read_place(access_token, universe_id, &universe.root_place_id)
                        .await
                    {
                        Ok(root) => places.push(root),
                        Err(error) => warnings.push(format!(
                            "Universe {universe_id} root place metadata was unavailable: {}.",
                            error.code
                        )),
                    }
                }
                places.sort_by(|left, right| {
                    right
                        .root
                        .cmp(&left.root)
                        .then_with(|| numeric_id_sort(&left.id, &right.id))
                });
                universe.places = places;
            }
            Err(error) => {
                warnings.push(format!(
                    "Universe {universe_id} place index was unavailable: {}. Only the exact root place was requested.",
                    error.code
                ));
                let root = self
                    .read_place(access_token, universe_id, &universe.root_place_id)
                    .await?;
                universe.places = vec![root];
            }
        }
        Ok(universe)
    }

    async fn read_place_index(&self, universe_id: &str) -> Result<Vec<RobloxPlaceMetadata>> {
        let mut places = BTreeMap::<String, RobloxPlaceMetadata>::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PLACE_INDEX_PAGES {
            let mut url = Url::parse(&format!(
                "{ROBLOX_DEVELOP_ORIGIN}/v1/universes/{universe_id}/places"
            ))
            .expect("fixed Roblox URL is valid");
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("limit", "100");
                query.append_pair("sortOrder", "Asc");
                if let Some(value) = cursor.as_deref() {
                    query.append_pair("cursor", value);
                }
            }
            let page: PlaceIndexWire = self
                .send_json(ResourceHttpRequest {
                    method: ResourceHttpMethod::Get,
                    url,
                    bearer: None,
                    form: Vec::new(),
                })
                .await?;
            for item in page.data {
                let place_id = item.id.to_string();
                let returned_universe = item.universe_id.to_string();
                if returned_universe != universe_id {
                    return Err(RobloxResourceError::new(
                        "ROBLOX_RESOURCE_PLACE_INDEX_PARENT",
                        RobloxResourceErrorKind::InvalidProviderResponse,
                        "Roblox place index returned an item outside its requested universe",
                        false,
                    ));
                }
                let place_id = validate_numeric_id(&place_id)?;
                let display_name = validate_display_text(item.name, "Roblox place name")?;
                let description = validate_optional_text(item.description)?;
                places
                    .entry(place_id.clone())
                    .or_insert(RobloxPlaceMetadata {
                        id: place_id.clone(),
                        universe_id: universe_id.to_owned(),
                        path: format!("universes/{universe_id}/places/{place_id}"),
                        display_name,
                        description,
                        root: false,
                        server_size: None,
                        create_time: None,
                        update_time: None,
                        oauth_validated: false,
                    });
                if places.len() > MAX_PLACES_PER_UNIVERSE {
                    return Err(RobloxResourceError::new(
                        "ROBLOX_RESOURCE_PLACE_LIMIT",
                        RobloxResourceErrorKind::InvalidProviderResponse,
                        "Roblox returned more places than this client can safely enumerate",
                        false,
                    ));
                }
            }
            cursor = validate_cursor(page.next_page_cursor)?;
            if cursor.is_none() {
                return Ok(places.into_values().collect());
            }
        }
        Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_PLACE_PAGINATION_LIMIT",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox place index exceeded the fixed pagination limit",
            false,
        ))
    }

    async fn send_json<R: DeserializeOwned>(&self, request: ResourceHttpRequest) -> Result<R> {
        validate_destination(request.url())?;
        let response = self.transport.send(request).await.map_err(|failure| {
            let (code, message, retryable) = match failure {
                ResourceTransportFailure::Timeout => (
                    "ROBLOX_RESOURCE_TIMEOUT",
                    "Roblox resource request timed out",
                    true,
                ),
                ResourceTransportFailure::BodyTooLarge => (
                    "ROBLOX_RESOURCE_BODY_TOO_LARGE",
                    "Roblox resource response exceeded the byte limit",
                    false,
                ),
                ResourceTransportFailure::Network | ResourceTransportFailure::Protocol => (
                    "ROBLOX_RESOURCE_TRANSPORT",
                    "Roblox resource transport failed",
                    true,
                ),
            };
            RobloxResourceError::new(
                code,
                RobloxResourceErrorKind::ProviderUnavailable,
                message,
                retryable,
            )
        })?;
        classify_status(response.status)?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_BODY_TOO_LARGE",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox resource response exceeded the byte limit",
                false,
            ));
        }
        serde_json::from_slice(&response.body).map_err(|_| {
            RobloxResourceError::new(
                "ROBLOX_RESOURCE_JSON_INVALID",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned malformed resource metadata",
                false,
            )
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenResourcesWire {
    #[serde(default)]
    resource_infos: Vec<ResourceInfoWire>,
}

#[derive(Debug, Deserialize)]
struct ResourceInfoWire {
    owner: ResourceOwnerWire,
    #[serde(default)]
    resources: BTreeMap<String, ResourceIdsWire>,
}

#[derive(Debug, Deserialize)]
struct ResourceOwnerWire {
    id: String,
    #[serde(rename = "type")]
    owner_type: String,
}

#[derive(Debug, Deserialize)]
struct ResourceIdsWire {
    #[serde(default)]
    ids: Vec<String>,
}

#[derive(Debug)]
struct AuthorizationSnapshot {
    owners: Vec<RobloxResourceOwner>,
    explicit_universe_ids: BTreeSet<String>,
    has_owner_wide_grant: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UniverseWire {
    path: String,
    create_time: Option<String>,
    update_time: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    user: Option<String>,
    group: Option<String>,
    visibility: Option<String>,
    root_place: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceWire {
    path: String,
    create_time: Option<String>,
    update_time: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    server_size: Option<u32>,
    root: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceIndexWire {
    #[serde(default)]
    data: Vec<PlaceIndexItemWire>,
    next_page_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceIndexItemWire {
    id: u64,
    universe_id: u64,
    name: Option<String>,
    description: Option<String>,
}

fn parse_authorization_snapshot(wire: TokenResourcesWire) -> Result<AuthorizationSnapshot> {
    if wire.resource_infos.len() > MAX_AUTHORIZATION_RECORDS {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_GRANT_LIMIT",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned too many authorization target records",
            false,
        ));
    }
    let mut owners = BTreeMap::<(String, String), RobloxResourceOwner>::new();
    let mut explicit_universe_ids = BTreeSet::new();
    let mut has_owner_wide_grant = false;
    for info in wire.resource_infos {
        if !matches!(info.owner.owner_type.as_str(), "User" | "Group") {
            return Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_OWNER_TYPE",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned an unsupported resource owner type",
                false,
            ));
        }
        let owner_id = validate_numeric_id(&info.owner.id)?;
        let owner = RobloxResourceOwner {
            id: owner_id.clone(),
            owner_type: info.owner.owner_type,
        };
        owners.insert((owner.owner_type.clone(), owner_id), owner);
        if let Some(universe) = info.resources.get("universe") {
            if universe.ids.len() > MAX_AUTHORIZED_UNIVERSES.saturating_mul(4) {
                return Err(RobloxResourceError::new(
                    "ROBLOX_RESOURCE_GRANT_ID_LIMIT",
                    RobloxResourceErrorKind::InvalidProviderResponse,
                    "Roblox returned too many universe target identifiers",
                    false,
                ));
            }
            for id in &universe.ids {
                if id == "U" {
                    has_owner_wide_grant = true;
                } else {
                    explicit_universe_ids.insert(validate_numeric_id(id)?);
                }
            }
        }
    }
    Ok(AuthorizationSnapshot {
        owners: owners.into_values().collect(),
        explicit_universe_ids,
        has_owner_wide_grant,
    })
}

fn authorize_universe(grants: &AuthorizationSnapshot, universe_id: &str) -> Result<()> {
    if grants.has_owner_wide_grant || grants.explicit_universe_ids.contains(universe_id) {
        Ok(())
    } else {
        Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_NOT_GRANTED",
            RobloxResourceErrorKind::AccessDenied,
            "The OAuth token does not grant the requested universe",
            false,
        ))
    }
}

fn universe_from_wire(universe_id: &str, wire: UniverseWire) -> Result<RobloxUniverseMetadata> {
    if wire.path != format!("universes/{universe_id}") {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_UNIVERSE_PATH",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned a mismatched universe resource path",
            false,
        ));
    }
    let root_place = wire.root_place.ok_or_else(|| {
        RobloxResourceError::new(
            "ROBLOX_RESOURCE_ROOT_PLACE_MISSING",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox universe metadata omitted its root place",
            false,
        )
    })?;
    let root_place_id = parse_place_path(universe_id, &root_place)?;
    if wire.user.is_some() && wire.group.is_some() {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_CREATOR_AMBIGUOUS",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned multiple universe creator types",
            false,
        ));
    }
    let creator = wire
        .user
        .map(|value| validate_creator_path("users", value))
        .or_else(|| {
            wire.group
                .map(|value| validate_creator_path("groups", value))
        })
        .transpose()?;
    Ok(RobloxUniverseMetadata {
        id: universe_id.to_owned(),
        path: wire.path,
        display_name: validate_display_text(wire.display_name, "Roblox universe name")?,
        description: validate_optional_text(wire.description)?,
        visibility: validate_optional_enum(wire.visibility)?,
        creator,
        root_place_id,
        create_time: validate_optional_timestamp(wire.create_time)?,
        update_time: validate_optional_timestamp(wire.update_time)?,
        places: Vec::new(),
    })
}

fn place_from_wire(
    universe_id: &str,
    place_id: &str,
    wire: PlaceWire,
) -> Result<RobloxPlaceMetadata> {
    if wire.path != format!("universes/{universe_id}/places/{place_id}") {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_PLACE_PATH",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned a mismatched place resource path",
            false,
        ));
    }
    Ok(RobloxPlaceMetadata {
        id: place_id.to_owned(),
        universe_id: universe_id.to_owned(),
        path: wire.path,
        display_name: validate_display_text(wire.display_name, "Roblox place name")?,
        description: validate_optional_text(wire.description)?,
        root: wire.root.unwrap_or(false),
        server_size: wire.server_size,
        create_time: validate_optional_timestamp(wire.create_time)?,
        update_time: validate_optional_timestamp(wire.update_time)?,
        oauth_validated: true,
    })
}

fn fixed_api_url(path: &str) -> Result<Url> {
    let url = Url::parse(&format!("{ROBLOX_API_ORIGIN}{path}")).map_err(|_| origin_error())?;
    validate_destination(&url)?;
    Ok(url)
}

fn validate_destination(url: &Url) -> Result<()> {
    let valid_origin = matches!(
        (url.scheme(), url.host_str(), url.port()),
        (
            "https",
            Some("apis.roblox.com" | "develop.roblox.com"),
            None
        )
    );
    if !valid_origin
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(origin_error());
    }
    Ok(())
}

fn origin_error() -> RobloxResourceError {
    RobloxResourceError::new(
        "ROBLOX_RESOURCE_ORIGIN_REJECTED",
        RobloxResourceErrorKind::InvalidRequest,
        "Roblox resource request escaped the pinned HTTPS origins",
        false,
    )
}

fn classify_status(status: u16) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        401 => Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_REAUTH_REQUIRED",
            RobloxResourceErrorKind::ReauthenticationRequired,
            "Roblox rejected the protected OAuth access token",
            false,
        )),
        403 | 404 => Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_ACCESS_DENIED",
            RobloxResourceErrorKind::AccessDenied,
            "Roblox denied access to the requested resource",
            false,
        )),
        429 | 500..=599 => Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_PROVIDER_UNAVAILABLE",
            RobloxResourceErrorKind::ProviderUnavailable,
            "Roblox resource service is unavailable or throttling requests",
            true,
        )),
        300..=399 => Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_REDIRECT_REJECTED",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox attempted to redirect a pinned resource request",
            false,
        )),
        _ => Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_RESPONSE_REJECTED",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned an unsuccessful resource response",
            false,
        )),
    }
}

fn validate_client_id(value: &str) -> Result<()> {
    if value.len() < 3
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_CLIENT_ID_INVALID",
            RobloxResourceErrorKind::InvalidRequest,
            "Roblox public client ID is malformed",
            false,
        ));
    }
    Ok(())
}

fn validate_numeric_id(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_ID_INVALID",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox resource identifier was malformed",
            false,
        ));
    }
    Ok(value.to_owned())
}

fn parse_place_path(universe_id: &str, path: &str) -> Result<String> {
    path.strip_prefix(&format!("universes/{universe_id}/places/"))
        .ok_or_else(|| {
            RobloxResourceError::new(
                "ROBLOX_RESOURCE_ROOT_PLACE_PATH",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned a malformed root place path",
                false,
            )
        })
        .and_then(validate_numeric_id)
}

fn validate_creator_path(prefix: &str, value: String) -> Result<String> {
    let id = value.strip_prefix(&format!("{prefix}/")).ok_or_else(|| {
        RobloxResourceError::new(
            "ROBLOX_RESOURCE_CREATOR_PATH",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned a malformed universe creator path",
            false,
        )
    })?;
    Ok(format!("{prefix}/{}", validate_numeric_id(id)?))
}

fn validate_display_text(value: Option<String>, _label: &str) -> Result<String> {
    let value = value.unwrap_or_else(|| "Untitled Roblox resource".to_owned());
    if value.len() > 512 || value.chars().any(|character| character == '\0') {
        return Err(RobloxResourceError::new(
            "ROBLOX_RESOURCE_TEXT_INVALID",
            RobloxResourceErrorKind::InvalidProviderResponse,
            "Roblox returned invalid resource display text",
            false,
        ));
    }
    Ok(value)
}

fn validate_optional_text(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(value) if value.len() > 16_384 || value.chars().any(|character| character == '\0') => {
            Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_DESCRIPTION_INVALID",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned invalid resource description text",
                false,
            ))
        }
        value => Ok(value),
    }
}

fn validate_optional_timestamp(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(value)
            if value.len() > 64
                || value.is_empty()
                || !value.is_ascii()
                || value.chars().any(char::is_control) =>
        {
            Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_TIMESTAMP_INVALID",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned an invalid resource timestamp",
                false,
            ))
        }
        value => Ok(value),
    }
}

fn validate_optional_enum(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(value)
            if value.is_empty()
                || value.len() > 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_') =>
        {
            Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_ENUM_INVALID",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned an invalid resource enum",
                false,
            ))
        }
        value => Ok(value),
    }
}

fn validate_cursor(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(value)
            if value.is_empty()
                || value.len() > 512
                || !value.is_ascii()
                || value.chars().any(char::is_control) =>
        {
            Err(RobloxResourceError::new(
                "ROBLOX_RESOURCE_CURSOR_INVALID",
                RobloxResourceErrorKind::InvalidProviderResponse,
                "Roblox returned an invalid place-index cursor",
                false,
            ))
        }
        value => Ok(value),
    }
}

fn numeric_id_sort(left: &str, right: &str) -> std::cmp::Ordering {
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use super::*;

    #[derive(Clone, Default)]
    struct FakeTransport {
        responses: Arc<Mutex<VecDeque<ResourceHttpResponse>>>,
        requests: Arc<Mutex<Vec<ResourceHttpRequest>>>,
    }

    impl FakeTransport {
        fn with(responses: Vec<ResourceHttpResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl RobloxResourceTransport for FakeTransport {
        async fn send(
            &self,
            request: ResourceHttpRequest,
        ) -> std::result::Result<ResourceHttpResponse, ResourceTransportFailure> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(ResourceTransportFailure::Protocol)
        }
    }

    fn json(value: serde_json::Value) -> ResourceHttpResponse {
        ResourceHttpResponse {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    fn grants(ids: &[&str]) -> ResourceHttpResponse {
        json(serde_json::json!({
            "resource_infos": [{
                "owner": {"id": "42", "type": "User"},
                "resources": {"universe": {"ids": ids}}
            }]
        }))
    }

    fn universe(id: &str, root: &str) -> ResourceHttpResponse {
        json(serde_json::json!({
            "path": format!("universes/{id}"),
            "displayName": format!("Universe {id}"),
            "description": "Provider metadata",
            "user": "users/42",
            "visibility": "PRIVATE",
            "rootPlace": format!("universes/{id}/places/{root}"),
            "createTime": "2026-01-01T00:00:00Z",
            "updateTime": "2026-02-01T00:00:00Z"
        }))
    }

    fn place_index(universe: u64, places: &[(u64, &str)]) -> ResourceHttpResponse {
        json(serde_json::json!({
            "data": places.iter().map(|(id, name)| serde_json::json!({
                "id": id,
                "universeId": universe,
                "name": name,
                "description": "Indexed metadata"
            })).collect::<Vec<_>>(),
            "nextPageCursor": null
        }))
    }

    fn place(universe: &str, id: &str, root: bool) -> ResourceHttpResponse {
        json(serde_json::json!({
            "path": format!("universes/{universe}/places/{id}"),
            "displayName": format!("Place {id}"),
            "description": "Exact OAuth metadata",
            "serverSize": 40,
            "root": root,
            "createTime": "2026-01-01T00:00:00Z",
            "updateTime": "2026-03-01T00:00:00Z"
        }))
    }

    #[tokio::test]
    async fn discovery_uses_exact_endpoints_and_returns_deterministic_inventory() {
        let transport = FakeTransport::with(vec![
            grants(&["200"]),
            universe("200", "201"),
            place_index(200, &[(202, "Arena"), (201, "Lobby")]),
        ]);
        let requests = transport.requests.clone();
        let provider = RobloxResourceProvider::new(transport);
        let inventory = provider
            .discover(&OAuthSecret::new("access-secret").unwrap(), "client-123")
            .await
            .unwrap();

        assert_eq!(inventory.universes.len(), 1);
        assert_eq!(inventory.universes[0].id, "200");
        assert_eq!(inventory.universes[0].places.len(), 2);
        assert_eq!(inventory.universes[0].places[0].id, "201");
        assert!(inventory.universes[0].places[0].root);
        assert!(!inventory.capabilities.desktop_place_publishing);

        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].url().as_str(), ROBLOX_TOKEN_RESOURCES_ENDPOINT);
        assert_eq!(requests[0].method(), ResourceHttpMethod::PostForm);
        assert_eq!(
            requests[1].url().as_str(),
            "https://apis.roblox.com/cloud/v2/universes/200"
        );
        assert_eq!(
            requests[2].url().as_str(),
            "https://develop.roblox.com/v1/universes/200/places?limit=100&sortOrder=Asc"
        );
        let debug = format!("{:?}", requests[0]);
        assert!(!debug.contains("access-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn exact_selection_is_oauth_validated_and_provider_derived() {
        let provider = RobloxResourceProvider::new(FakeTransport::with(vec![
            grants(&["200"]),
            universe("200", "201"),
            place("200", "202", false),
        ]));
        let evidence = provider
            .read_selection(
                &OAuthSecret::new("access-secret").unwrap(),
                "client-123",
                "200",
                "202",
                1_750_000_000,
            )
            .await
            .unwrap();
        assert_eq!(evidence.place.path, "universes/200/places/202");
        assert!(evidence.place.oauth_validated);
        assert_eq!(
            evidence.universe.update_time.as_deref(),
            Some("2026-02-01T00:00:00Z")
        );
        assert_eq!(evidence.observed_at_unix_seconds, 1_750_000_000);
        assert_eq!(
            evidence.place_publishing_credential,
            "operator-api-key-only"
        );
    }

    #[tokio::test]
    async fn owner_wide_grant_is_explicitly_partial_and_supports_exact_probe() {
        let provider = RobloxResourceProvider::new(FakeTransport::with(vec![grants(&["U"])]));
        let inventory = provider
            .discover(&OAuthSecret::new("access-secret").unwrap(), "client-123")
            .await
            .unwrap();
        assert!(inventory.has_owner_wide_grant);
        assert!(inventory.universes.is_empty());
        assert_eq!(inventory.warnings.len(), 1);

        let provider = RobloxResourceProvider::new(FakeTransport::with(vec![
            grants(&["U"]),
            universe("900", "901"),
            place_index(900, &[(901, "Root")]),
        ]));
        let probed = provider
            .probe_universe(
                &OAuthSecret::new("access-secret").unwrap(),
                "client-123",
                "900",
            )
            .await
            .unwrap();
        assert_eq!(probed.id, "900");
    }

    #[tokio::test]
    async fn ungranted_universe_stops_before_metadata_read() {
        let transport = FakeTransport::with(vec![grants(&["200"])]);
        let requests = transport.requests.clone();
        let provider = RobloxResourceProvider::new(transport);
        let error = provider
            .read_selection(
                &OAuthSecret::new("access-secret").unwrap(),
                "client-123",
                "999",
                "1000",
                1,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, RobloxResourceErrorKind::AccessDenied);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn mismatched_place_path_and_redirect_status_fail_closed() {
        let provider = RobloxResourceProvider::new(FakeTransport::with(vec![
            grants(&["200"]),
            universe("200", "201"),
            place("999", "202", false),
        ]));
        let error = provider
            .read_selection(
                &OAuthSecret::new("access-secret").unwrap(),
                "client-123",
                "200",
                "202",
                1,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "ROBLOX_RESOURCE_PLACE_PATH");

        let provider =
            RobloxResourceProvider::new(FakeTransport::with(vec![ResourceHttpResponse {
                status: 302,
                body: Vec::new(),
            }]));
        let error = provider
            .discover(&OAuthSecret::new("access-secret").unwrap(), "client-123")
            .await
            .unwrap_err();
        assert_eq!(error.code, "ROBLOX_RESOURCE_REDIRECT_REJECTED");
    }

    #[test]
    fn only_two_exact_https_origins_are_allowed() {
        assert!(validate_destination(&Url::parse(ROBLOX_API_ORIGIN).unwrap()).is_ok());
        assert!(validate_destination(&Url::parse(ROBLOX_DEVELOP_ORIGIN).unwrap()).is_ok());
        assert!(
            validate_destination(&Url::parse("https://apis.roblox.com.evil.test/").unwrap())
                .is_err()
        );
        assert!(
            validate_destination(&Url::parse("https://apis.roblox.com:444/").unwrap()).is_err()
        );
        assert!(validate_destination(&Url::parse("http://apis.roblox.com/").unwrap()).is_err());
    }
}
