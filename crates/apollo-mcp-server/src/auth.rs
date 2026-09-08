use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, authorization::Bearer},
};
use http::Method;
use networked_key_resolver::{CachedJwks, InflightMap, IssuerFetchState, NetworkedKeyResolver};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tracing::warn;
use url::Url;

use crate::apps::app_param_from_query;
use crate::scope_requirements::OperationRequiredScopes;

mod networked_key_resolver;
mod protected_resource;
mod valid_token;
mod www_authenticate;

use protected_resource::ProtectedResource;
use valid_token::TokenValidator;
pub(crate) use valid_token::ValidToken;
use www_authenticate::{BearerError, WwwAuthenticate};

/// Enforcement mode for the global OAuth scope requirement on authenticated
/// requests.
///
/// This mode only affects the global `scopes` list. Per-operation scope
/// requirements are checked separately and always require all of that
/// operation's scopes.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMode {
    /// Skip the global scope requirement. Per-operation requirements still apply.
    Disabled,
    /// Token must have ALL configured scopes (default).
    #[default]
    RequireAll,
    /// Token must have at least ONE configured scope.
    RequireAny,
}

impl ScopeMode {
    /// Whether the `present` scopes satisfy the global `required` scopes under
    /// this mode. Callers skip this check when no global scopes are configured,
    /// so `required` is expected to be non-empty.
    fn is_satisfied_by(self, required: &[String], present: &[String]) -> bool {
        match self {
            ScopeMode::Disabled => true,
            ScopeMode::RequireAll => required.iter().all(|req| present.contains(req)),
            ScopeMode::RequireAny => required.iter().any(|req| present.contains(req)),
        }
    }

    /// The wire string for this mode, matching the serde `snake_case` rename.
    /// Used when rendering the `scope_mode` hint in the `WWW-Authenticate`
    /// header.
    fn as_str(self) -> &'static str {
        match self {
            ScopeMode::Disabled => "disabled",
            ScopeMode::RequireAll => "require_all",
            ScopeMode::RequireAny => "require_any",
        }
    }
}

/// Errors that can occur when building a TLS-configured HTTP client
#[derive(Debug, thiserror::Error)]
pub enum TlsConfigError {
    #[error("Failed to read CA certificate from {path}: {source}")]
    CertificateRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Failed to parse CA certificate from {path}: invalid PEM format")]
    CertificateParse { path: PathBuf },
    #[error("Failed to build HTTP client: {0}")]
    ClientBuild(#[from] reqwest::Error),
    #[error("Auth server URL at index {index} ({url}) has no host")]
    ServerUrlMissingHost { index: usize, url: String },
    #[error("Resource URL has non-HTTP(S) scheme '{scheme}': {url}")]
    ResourceUrlInvalidScheme { url: String, scheme: String },
    #[error(
        "transport.auth sets both `allow_anonymous_mcp_discovery` and \
         `skip_token_validation.methods`. `allow_anonymous_mcp_discovery` is deprecated: remove it \
         and list the methods you want in `skip_token_validation.methods`."
    )]
    AnonymousDiscoveryConflict,
}

impl TlsConfig {
    /// Build a reqwest client configured with the TLS settings and default headers
    pub fn build_client(
        &self,
        default_headers: HeaderMap,
    ) -> Result<reqwest::Client, TlsConfigError> {
        let mut builder = reqwest::Client::builder().default_headers(default_headers);

        // Add custom CA certificate if provided
        if let Some(ca_cert_path) = &self.ca_cert {
            let cert_bytes =
                std::fs::read(ca_cert_path).map_err(|e| TlsConfigError::CertificateRead {
                    path: ca_cert_path.clone(),
                    source: e,
                })?;
            let cert = reqwest::Certificate::from_pem(&cert_bytes).map_err(|_| {
                TlsConfigError::CertificateParse {
                    path: ca_cert_path.clone(),
                }
            })?;
            builder = builder.add_root_certificate(cert);
            tracing::debug!("Added custom CA certificate from {:?}", ca_cert_path);
        }

        // Accept invalid certs if configured (development only)
        if self.danger_accept_invalid_certs {
            tracing::warn!(
                "TLS certificate validation is disabled. This is insecure and should only be used for development."
            );
            builder = builder.danger_accept_invalid_certs(true);
        }

        Ok(builder.build()?)
    }
}

/// Deserialize a `HeaderMap` from a map of string keys and values.
fn deserialize_header_map<'de, D>(deserializer: D) -> Result<HeaderMap, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let map = HashMap::<String, String>::deserialize(deserializer)?;
    let mut headers = HeaderMap::with_capacity(map.len());
    for (key, value) in map {
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|e| serde::de::Error::custom(e.to_string()))?;
        let value =
            HeaderValue::from_str(&value).map_err(|e| serde::de::Error::custom(e.to_string()))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn deserialize_auth_servers<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let servers: Vec<String> = Vec::deserialize(d)?;
    for s in &servers {
        Url::parse(s)
            .map_err(|e| D::Error::custom(format!("invalid auth server URL {s:?}: {e}")))?;
    }
    Ok(servers)
}

/// The JSON-RPC method every tool call shares.
const TOOL_CALL_METHOD: &str = "tools/call";

/// JSON-RPC methods that resolve one of many resources or prompts through a
/// request parameter, the same shape as `tools/call`, but with no per-item
/// list like `skip_token_validation.tools` to redirect to. `resources/subscribe`,
/// `resources/unsubscribe`, and `completion/complete` are not served today, but
/// are included so the guardrail does not need a revisit when support lands.
const RESOLVES_ONE_OF_MANY_METHODS: &[&str] = &[
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
    "prompts/get",
    "completion/complete",
];

fn deserialize_skip_methods<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let methods: Vec<String> = Vec::deserialize(d)?;
    if methods.iter().any(|method| method == TOOL_CALL_METHOD) {
        return Err(D::Error::custom(format!(
            "`{TOOL_CALL_METHOD}` cannot appear in `skip_token_validation.methods`, because every \
             tool call uses that one method name and listing it would expose every tool. List the \
             individual tool names in `skip_token_validation.tools` instead."
        )));
    }
    if let Some(method) = methods
        .iter()
        .find(|method| RESOLVES_ONE_OF_MANY_METHODS.contains(&method.as_str()))
    {
        return Err(D::Error::custom(format!(
            "`{method}` cannot appear in `skip_token_validation.methods`, because it resolves one \
             of many resources or prompts through a request parameter, and listing the method \
             itself would expose all of them without a token."
        )));
    }
    Ok(methods)
}

fn deserialize_header_names<'de, D>(d: D) -> Result<Vec<HeaderName>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    Vec::<String>::deserialize(d)?
        .into_iter()
        .map(|name| {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| D::Error::custom(format!("invalid header name {name:?}: {e}")))?;
            if header_name == http::header::AUTHORIZATION {
                return Err(D::Error::custom(
                    "`authorization` cannot appear in `skip_token_validation.headers`: every \
                     skip list applies only to a request that carries no `Authorization` header, \
                     so this entry could never match",
                ));
            }
            Ok(header_name)
        })
        .collect()
}

/// Requests that skip OAuth token validation.
///
/// Every list applies only to a request that carries no `Authorization` header.
/// A request that presents a token is always validated, so an expired token is
/// rejected even when the request matches a list.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkipTokenValidation {
    /// JSON-RPC method names, such as `tools/list`.
    ///
    /// `tools/call` is rejected here, because every tool call uses that one
    /// method name. Name the tools in `tools` instead. Other methods that name
    /// one of many items through a request parameter, such as `resources/read`
    /// and `prompts/get`, are rejected for the same reason.
    #[serde(default, deserialize_with = "deserialize_skip_methods")]
    pub methods: Vec<String>,

    /// Tool names, matched against the requested tool of a `tools/call`. This
    /// is the only list that tells one tool from another.
    ///
    /// A call carrying an `?app=` query parameter never matches, because the
    /// dispatcher resolves that app's own tool rather than the operation this
    /// name refers to.
    #[serde(default)]
    pub tools: Vec<String>,

    /// HTTP header names, such as `x-api-key`.
    ///
    /// A request that carries a listed header skips token validation, and a
    /// later layer authenticates it. This moves authentication elsewhere rather
    /// than removing it, because the middleware cannot judge a credential it
    /// does not understand.
    ///
    /// A match here is decided before the body is read, so it skips validation
    /// for every method and tool, including ones absent from `methods` and
    /// `tools`. It does not scope down to only those lists.
    #[serde(default, deserialize_with = "deserialize_header_names")]
    #[schemars(with = "Vec<String>")]
    pub headers: Vec<HeaderName>,
}

impl SkipTokenValidation {
    /// Whether deciding a match needs the JSON-RPC body.
    fn needs_body(&self) -> bool {
        !self.methods.is_empty() || !self.tools.is_empty()
    }

    fn matches_headers(&self, headers: &HeaderMap) -> bool {
        self.headers.iter().any(|name| headers.contains_key(name))
    }

    fn matches_body(&self, peek: &JsonRpcBodyPeek, app_qualified: bool) -> bool {
        if self.methods.iter().any(|method| method == &peek.method) {
            return true;
        }
        if peek.method != TOOL_CALL_METHOD {
            return false;
        }
        // An `?app=` query parameter makes the dispatcher resolve the app's own
        // tool rather than the operation this name refers to, so a name in
        // `tools` does not identify what would actually run.
        if app_qualified {
            return false;
        }
        peek.params
            .as_ref()
            .and_then(|params| params.name.as_deref())
            .is_some_and(|tool| self.tools.iter().any(|allowed| allowed == tool))
    }
}

/// Auth configuration options
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// List of upstream OAuth servers to delegate auth.
    /// Not `Vec<Url>`: `Url::parse` appends `/` to bare-authority inputs,
    /// breaking issuer string-matching in clients.
    #[serde(deserialize_with = "deserialize_auth_servers")]
    #[schemars(with = "Vec<Url>")]
    pub servers: Vec<String>,

    /// List of accepted audiences for the OAuth tokens
    #[serde(default)]
    pub audiences: Vec<String>,

    /// Optional allowlist for token issuers (the JWT `iss` claim).
    /// Not `Vec<Url>`: `Url::parse` appends `/` to bare-authority inputs,
    /// breaking exact `iss` string matching.
    ///
    /// When non-empty, a token's `iss` claim must match one of these values and
    /// the discovered issuer of the server whose key verified it. When empty
    /// (default), these token issuer checks are skipped; authorization-server
    /// metadata issuer validation still applies.
    #[serde(default)]
    pub issuers: Vec<String>,

    /// Allow any audience (skip validation) - use with caution
    #[serde(default)]
    pub allow_any_audience: bool,

    /// The resource to protect.
    ///
    /// Note: This is usually the publicly accessible URL of this running MCP server
    pub resource: Url,

    /// Link to documentation related to the protected resource
    pub resource_documentation: Option<Url>,

    /// Supported OAuth scopes by this resource server
    pub scopes: Vec<String>,

    /// Global scope enforcement mode: disabled, require_all (default), or require_any.
    /// This does not change the all-of semantics of per-operation requirements.
    #[serde(default)]
    pub scope_mode: ScopeMode,

    /// Whether to disable the auth token passthrough to upstream API
    #[serde(default)]
    pub disable_auth_token_passthrough: bool,

    /// Requests that skip OAuth token validation.
    ///
    /// Each list keys on a different part of the request: JSON-RPC method name,
    /// requested tool name, or HTTP header name. An empty list is off, so there
    /// is no separate switch to enable them.
    #[serde(default)]
    pub skip_token_validation: SkipTokenValidation,

    /// Deprecated. Use `skip_token_validation.methods` instead.
    ///
    /// Enabling this is the same as listing `initialize`, `tools/list`, and
    /// `resources/list` in `skip_token_validation.methods`. Setting both is an
    /// error, because the two would describe the same list twice.
    #[serde(default)]
    #[deprecated(
        since = "1.18.0",
        note = "use `skip_token_validation.methods: [\"initialize\", \"tools/list\", \"resources/list\"]` instead"
    )]
    pub allow_anonymous_mcp_discovery: bool,

    /// TLS configuration for connecting to OAuth servers
    #[serde(default)]
    pub tls: TlsConfig,

    /// Timeout for OIDC discovery requests.
    ///
    /// Accepts human-readable durations (e.g., "5s", "10s", "30s").
    /// Defaults to 5 seconds when not specified.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>")]
    pub discovery_timeout: Option<Duration>,

    /// Headers to include in OIDC discovery and JWKS requests.
    ///
    /// Use this to set headers like `User-Agent` that may be required
    /// by upstream OAuth servers or web application firewalls.
    #[serde(default, deserialize_with = "deserialize_header_map")]
    #[schemars(with = "HashMap<String, String>")]
    pub discovery_headers: HeaderMap,
}

/// TLS configuration for OAuth server connections
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Path to additional CA certificates to trust (PEM format).
    /// Use this when your OAuth server uses a self-signed certificate
    /// or a certificate signed by a private CA.
    pub ca_cert: Option<PathBuf>,

    /// Whether to accept invalid TLS certificates.
    ///
    /// **WARNING**: This is insecure and should only be used for development/testing.
    /// When enabled, the server will accept any certificate, including self-signed
    /// and expired certificates, without validation.
    #[serde(default)]
    pub danger_accept_invalid_certs: bool,
}

/// Joins a URL's path into its non-empty segments separated by `/`, dropping
/// leading and trailing slashes. Shared by the protected-resource and discovery
/// well-known URL builders.
fn normalized_path_segments(url: &Url) -> String {
    url.path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Constructs the protected resource metadata URL per RFC 9728 Section 3.
///
/// The well-known URI is formed by inserting `/.well-known/oauth-protected-resource`
/// between the host and path components of the resource identifier.
/// Query strings and fragments are stripped per RFC 9728.
fn build_resource_metadata_url(resource: &Url) -> Url {
    let mut url = resource.clone();
    url.set_query(None);
    url.set_fragment(None);

    if url.host_str().is_none() {
        warn!("resource URL has no host, falling back to root-level metadata path");
        url.set_path("/.well-known/oauth-protected-resource");
        return url;
    }

    let path = normalized_path_segments(&url);

    if path.is_empty() {
        url.set_path("/.well-known/oauth-protected-resource");
    } else {
        url.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
    }

    url
}

/// Internal state for the auth middleware, containing both config and pre-built HTTP client
#[derive(Clone)]
struct AuthState {
    config: Arc<Config>,
    client: reqwest::Client,
    jwks_cache: Arc<RwLock<HashMap<Url, CachedJwks>>>,
    inflight: Arc<InflightMap>,
    resource_metadata_url: Url,
    /// Upstream OAuth server URLs, parsed once at startup so the per-request
    /// path neither re-parses nor allocates them.
    auth_servers: Arc<[Url]>,
    /// Per-operation required scopes, keyed by the exact MCP tool name used in
    /// `tools/call`.
    /// Missing keys impose no additional restriction beyond the global requirement.
    required_scopes: Arc<HashMap<String, OperationRequiredScopes>>,
    /// Skip lists resolved at startup, with the deprecated
    /// `allow_anonymous_mcp_discovery` flag already folded in.
    skip_token_validation: Arc<SkipTokenValidation>,
}

impl Config {
    /// Folds the deprecated `allow_anonymous_mcp_discovery` flag into
    /// `skip_token_validation`, so the request path reads a single list.
    #[allow(deprecated)]
    fn resolve_skip_token_validation(&self) -> Result<SkipTokenValidation, TlsConfigError> {
        if !self.allow_anonymous_mcp_discovery {
            return Ok(self.skip_token_validation.clone());
        }
        if !self.skip_token_validation.methods.is_empty() {
            return Err(TlsConfigError::AnonymousDiscoveryConflict);
        }

        warn!(
            "allow_anonymous_mcp_discovery is deprecated. Replace it with \
             `skip_token_validation.methods: {DEPRECATED_ANONYMOUS_DISCOVERY_METHODS:?}`."
        );
        let mut resolved = self.skip_token_validation.clone();
        resolved.methods = DEPRECATED_ANONYMOUS_DISCOVERY_METHODS
            .iter()
            .map(|method| (*method).to_string())
            .collect();
        Ok(resolved)
    }

    /// Enable auth middleware on the router.
    ///
    /// Builds the HTTP client at startup to validate TLS configuration eagerly.
    pub fn enable_middleware(
        &self,
        router: Router,
        required_scopes: HashMap<String, OperationRequiredScopes>,
    ) -> Result<Router, TlsConfigError> {
        // Parse and validate server URLs once at startup (fail fast on config
        // errors). The parsed list is reused for every request via `AuthState`.
        #[allow(clippy::expect_used)] // parseability validated at deserialize
        let auth_servers = self
            .servers
            .iter()
            .enumerate()
            .map(|(i, server)| {
                let parsed = Url::parse(server).expect("validated by deserialize_auth_servers");
                if parsed.host_str().is_none() {
                    return Err(TlsConfigError::ServerUrlMissingHost {
                        index: i,
                        url: server.clone(),
                    });
                }
                Ok(parsed)
            })
            .collect::<Result<Vec<Url>, _>>()?;

        let scheme = self.resource.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(TlsConfigError::ResourceUrlInvalidScheme {
                url: self.resource.to_string(),
                scheme: scheme.to_string(),
            });
        }
        if self.allow_any_audience {
            warn!(
                "allow_any_audience is enabled - audience validation is disabled. This reduces security."
            );
        }

        if self.scope_mode == ScopeMode::Disabled && !self.scopes.is_empty() {
            warn!(
                "scope_mode is 'disabled' but scopes are configured - global scope enforcement will be skipped"
            );
        }

        /// Simple handler to encode our config into the desired OAuth 2.1 protected
        /// resource format
        async fn protected_resource(
            State(auth_state): State<AuthState>,
        ) -> Json<ProtectedResource> {
            Json(ProtectedResource::from(auth_state.config.as_ref().clone()))
        }

        let skip_token_validation = self.resolve_skip_token_validation()?;
        for tool in &skip_token_validation.tools {
            if let Some(required) = required_scopes.get(tool) {
                warn!(
                    tool,
                    ?required,
                    "tool is in skip_token_validation.tools, so its required scopes are not enforced for requests without a token"
                );
            }
        }
        if !skip_token_validation.headers.is_empty()
            && (!required_scopes.is_empty()
                || (self.scope_mode != ScopeMode::Disabled && !self.scopes.is_empty()))
        {
            warn!(
                headers = ?skip_token_validation.headers,
                scoped_tools = required_scopes.len(),
                global_scopes = ?self.scopes,
                "a request carrying one of these headers skips token validation for every tool, \
                 voiding any per-operation or global scope requirement for it"
            );
        }

        // Build HTTP client with TLS configuration and discovery headers
        let client = self.tls.build_client(self.discovery_headers.clone())?;
        let resource_metadata_url = build_resource_metadata_url(&self.resource);
        let metadata_route_path = resource_metadata_url.path().to_string();
        let auth_state = AuthState {
            config: Arc::new(self.clone()),
            client,
            resource_metadata_url,
            auth_servers: Arc::from(auth_servers),
            required_scopes: Arc::new(required_scopes),
            skip_token_validation: Arc::new(skip_token_validation),
            inflight: Arc::new(Mutex::new(IssuerFetchState::default())),
            jwks_cache: Arc::new(RwLock::new(HashMap::new())),
        };

        // Set up auth routes. NOTE: CORs needs to allow for get requests to the
        // metadata information paths.
        let cors = CorsLayer::new()
            .allow_methods([Method::GET])
            .allow_origin(Any);
        let auth_router = Router::new()
            .route(&metadata_route_path, get(protected_resource))
            .with_state(auth_state.clone())
            .layer(cors);

        // Merge with MCP server routes
        Ok(Router::new().merge(auth_router).merge(router.layer(
            axum::middleware::from_fn_with_state(auth_state, oauth_validate),
        )))
    }
}

/// Default timeout for OIDC/OAuth discovery and JWKS requests when
/// `transport.auth.discovery_timeout` is not configured.
const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Fixed TTL for cached JWKS entries; not operator-configurable.
const DEFAULT_JWKS_CACHE_TTL: Duration = Duration::from_secs(600); // 10 min

/// Fixed minimum interval between JWKS refreshes per issuer; not
/// operator-configurable.
const JWKS_MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The methods the deprecated `allow_anonymous_mcp_discovery` flag allows,
/// which it now expresses as `skip_token_validation.methods`.
const DEPRECATED_ANONYMOUS_DISCOVERY_METHODS: &[&str] =
    &["initialize", "tools/list", "resources/list"];

/// Maximum body size to buffer when peeking at the JSON-RPC method or tool
/// name. A discovery request such as `tools/list` is under 100 bytes, but a
/// tokenless `tools/call` against a skip-listed tool, or one a per-operation
/// scope check needs to inspect, shares this same peek, so 16 KiB also has to
/// hold real tool arguments. A body over this limit never fails a tokenless
/// request outright: the request still gets the normal 401 challenge below
/// instead of a confusing 413, since it was never going to succeed without a
/// token regardless of what this peek would have found.
const PEEK_BODY_LIMIT: usize = 16 * 1024;

/// Struct for deserializing the JSON-RPC `method` and optional `params.name`
/// from a request body. Used by both the method/tool skip lists and per-operation scope checks.
#[derive(Deserialize)]
struct JsonRpcBodyPeek {
    method: String,
    params: Option<JsonRpcParams>,
}

#[derive(Deserialize)]
struct JsonRpcParams {
    name: Option<String>,
}

async fn extract_body(request: &mut Request) -> Result<JsonRpcBodyPeek, StatusCode> {
    let body = std::mem::take(request.body_mut());

    let bytes = axum::body::to_bytes(body, PEEK_BODY_LIMIT)
        .await
        .inspect_err(
            |e| tracing::error!(error = %e, "Failed to read request body in oauth middleware"),
        )
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    let peek = serde_json::from_slice::<JsonRpcBodyPeek>(&bytes)
        .inspect_err(
            |e| tracing::error!(error = %e, "Failed to parse request body in oauth middleware"),
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    *request.body_mut() = axum::body::Body::from(bytes);

    Ok(peek)
}

/// Checks if a `tools/call` request is missing required scopes for the named operation.
///
/// Returns `Some(required_scopes)` when the token lacks one or more of the operation's
/// required scopes, and `None` when the check passes or does not apply. It does not apply
/// to non-`tools/call` methods, requests without a tool name, or tools with no entry in
/// `required_scopes` — those are governed only by the global scope requirement.
fn missing_scopes_for_operation<'a>(
    peek: &JsonRpcBodyPeek,
    required_scopes: &'a HashMap<String, OperationRequiredScopes>,
    token_scopes: &[String],
) -> Option<&'a OperationRequiredScopes> {
    if peek.method != "tools/call" {
        return None;
    }
    let op_name = peek.params.as_ref()?.name.as_deref()?;
    let required = required_scopes.get(op_name)?;
    if required.is_satisfied_by(token_scopes) {
        return None;
    }
    Some(required)
}

/// Validates bearer JWTs and configured scopes, except for requests that
/// match a configured `skip_token_validation` list.
#[tracing::instrument(skip_all, fields(status_code, reason))]
async fn oauth_validate(
    State(auth_state): State<AuthState>,
    token: Option<TypedHeader<Authorization<Bearer>>>,
    mut request: Request,
    next: Next,
) -> Result<Response, (StatusCode, TypedHeader<WwwAuthenticate>)> {
    let auth_config = &auth_state.config;
    let resource_metadata_url = &auth_state.resource_metadata_url;

    // Unauthorized error for missing or invalid tokens
    let unauthorized_error = || {
        let scope = if auth_config.scopes.is_empty() {
            None
        } else {
            Some(auth_config.scopes.join(" "))
        };

        (
            StatusCode::UNAUTHORIZED,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource_metadata_url.clone(),
                scope,
                error: None,
                scope_mode: Some(auth_config.scope_mode),
            }),
        )
    };

    // RFC 6750 insufficient-scope challenge. Advertise the full requirement so
    // clients know the target scope set, rather than only the scopes they lack.
    let forbidden_error = |required_scopes: &[String], scope_mode: Option<ScopeMode>| {
        (
            StatusCode::FORBIDDEN,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource_metadata_url.clone(),
                scope: Some(required_scopes.join(" ")),
                error: Some(BearerError::InsufficientScope),
                scope_mode,
            }),
        )
    };

    // Every skip list applies only to a request that presents no token. A caller
    // that sends one always gets it validated, so an expired token is rejected
    // here instead of passing as an anonymous request.
    let skip = &auth_state.skip_token_validation;

    // The header list needs no body, so it answers before the body is buffered.
    if token.is_none() && skip.matches_headers(request.headers()) {
        let response = next.run(request).await;
        tracing::Span::current().record("status_code", response.status().as_u16());
        return Ok(response);
    }

    // Read through the same helper the dispatcher uses, so both layers agree on
    // whether this request targets an app.
    let app_qualified = app_param_from_query(request.uri().query()).is_some();

    // Extract the body once if we need to inspect the JSON-RPC method for either
    // the method and tool skip lists or per-operation scope checks.
    let peek_for_skip = token.is_none() && skip.needs_body();
    let body_peek = if request.method() == http::Method::POST
        && (peek_for_skip || !auth_state.required_scopes.is_empty())
    {
        match extract_body(&mut request).await {
            Ok(peek) => Some(peek),
            // Without a token the request ends at the 401 challenge below
            // regardless of this peek's outcome, whether because no skip list
            // matches or because a per-operation scope check needs a token
            // that isn't there. Surfacing 413/400 here instead would hide
            // that reachable, actionable 401 behind an unrelated body-size
            // or parsing limit. `for_skip_lists` says which of those two
            // reasons triggered this peek, since only one of them involves
            // `skip_token_validation` at all.
            Err(status) if token.is_none() => {
                tracing::warn!(
                    peek_status = status.as_u16(),
                    for_skip_lists = peek_for_skip,
                    "body too large or malformed to peek while checking whether this \
                     tokenless request can proceed; falling through to normal token validation"
                );
                None
            }
            Err(status) => {
                tracing::Span::current().record("status_code", status.as_u16());
                return Ok(status.into_response());
            }
        }
    } else {
        None
    };

    if peek_for_skip
        && body_peek
            .as_ref()
            .is_some_and(|peek| skip.matches_body(peek, app_qualified))
    {
        let response = next.run(request).await;
        tracing::Span::current().record("status_code", response.status().as_u16());
        return Ok(response);
    }

    let discovery_timeout = auth_config
        .discovery_timeout
        .unwrap_or(DEFAULT_DISCOVERY_TIMEOUT);

    let validator = TokenValidator {
        audiences: &auth_config.audiences,
        issuers: &auth_config.issuers,
        allow_any_audience: auth_config.allow_any_audience,
        servers: &auth_state.auth_servers,
        keys: NetworkedKeyResolver::new(
            &auth_state.client,
            discovery_timeout,
            &auth_state.inflight,
            &auth_state.jwks_cache,
            DEFAULT_JWKS_CACHE_TTL,
            JWKS_MIN_REFRESH_INTERVAL,
        ),
    };
    let token = token.ok_or_else(|| {
        tracing::Span::current().record("reason", "missing_token");
        tracing::Span::current().record("status_code", StatusCode::UNAUTHORIZED.as_u16());
        unauthorized_error()
    })?;

    let valid_token = validator.validate(token.0).await.ok_or_else(|| {
        tracing::Span::current().record("reason", "invalid_token");
        tracing::Span::current().record("status_code", StatusCode::UNAUTHORIZED.as_u16());
        unauthorized_error()
    })?;

    // Global scope validation. An empty list or `scope_mode: disabled` imposes
    // no global scope requirement; per-operation requirements still run below.
    if !auth_config.scopes.is_empty() {
        let sufficient = auth_config
            .scope_mode
            .is_satisfied_by(&auth_config.scopes, &valid_token.scopes);

        if !sufficient {
            // Compute missing scopes for diagnostic logging
            let missing: Vec<_> = auth_config
                .scopes
                .iter()
                .filter(|req| !valid_token.scopes.contains(*req))
                .collect();

            tracing::warn!(
                required = ?auth_config.scopes,
                present = ?valid_token.scopes,
                missing = ?missing,
                mode = ?auth_config.scope_mode,
                "Token has insufficient scopes"
            );
            tracing::Span::current().record("reason", "insufficient_scope");
            tracing::Span::current().record("status_code", StatusCode::FORBIDDEN.as_u16());
            // NOTE: WWW-Authenticate lists all configured global scopes per RFC 6750.
            // In require_any mode, only one is needed, but the header format
            // doesn't distinguish. This matches existing behavior.
            return Err(forbidden_error(
                &auth_config.scopes,
                Some(auth_config.scope_mode),
            ));
        }
    }

    // Per-operation requirements add to the global check and always require
    // every listed scope, independently of the global `scope_mode`.
    if let Some(required) = body_peek.as_ref().and_then(|peek| {
        missing_scopes_for_operation(peek, &auth_state.required_scopes, &valid_token.scopes)
    }) {
        let challenge_scopes = required.challenge_scopes();
        tracing::warn!(
            required = ?required,
            present = ?valid_token.scopes,
            "Token has insufficient scopes for operation"
        );
        tracing::Span::current().record("reason", "insufficient_scope");
        tracing::Span::current().record("status_code", StatusCode::FORBIDDEN.as_u16());
        // Per-operation `required_scopes` groups are always a fully-required
        // AND set (that's the unit an alternative represents), regardless of
        // the global `scope_mode`, so the challenge reports `require_all`
        // rather than inheriting the global hint.
        return Err(forbidden_error(
            challenge_scopes,
            Some(ScopeMode::RequireAll),
        ));
    }

    // Insert new context to ensure that handlers only use our enforced token verification
    // for propagation
    request.extensions_mut().insert(valid_token);

    let response = next.run(request).await;
    tracing::Span::current().record("status_code", response.status().as_u16());
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
    };
    use http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
    use tower::ServiceExt; // for .oneshot()
    use url::Url;

    #[allow(deprecated)]
    fn test_config() -> Config {
        Config {
            servers: vec!["http://localhost:1234".to_string()],
            audiences: vec!["test-audience".to_string()],
            issuers: vec![],
            allow_any_audience: false,
            resource: Url::parse("http://localhost:4000").unwrap(),
            resource_documentation: None,
            scopes: vec!["read".to_string()],
            scope_mode: ScopeMode::default(),
            disable_auth_token_passthrough: false,
            skip_token_validation: SkipTokenValidation::default(),
            allow_anonymous_mcp_discovery: false,
            tls: TlsConfig::default(),
            discovery_timeout: None,
            discovery_headers: HeaderMap::new(),
        }
    }

    fn test_auth_state(config: Config) -> AuthState {
        let resource_metadata_url = build_resource_metadata_url(&config.resource);
        let auth_servers = config
            .servers
            .iter()
            .map(|s| Url::parse(s).expect("valid test server URL"))
            .collect::<Vec<_>>();
        // Resolve through the same path as `enable_middleware`, so tests cover
        // the deprecated-flag mapping rather than a hand-built list.
        let skip_token_validation = config
            .resolve_skip_token_validation()
            .expect("valid test skip lists");
        AuthState {
            config: Arc::new(config),
            client: reqwest::Client::new(),
            resource_metadata_url,
            auth_servers: Arc::from(auth_servers),
            required_scopes: Arc::new(HashMap::new()),
            skip_token_validation: Arc::new(skip_token_validation),
            inflight: Arc::new(Mutex::new(IssuerFetchState::default())),
            jwks_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn test_router(config: Config) -> Router {
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(from_fn_with_state(test_auth_state(config), oauth_validate))
    }

    fn test_router_with_required_scopes(
        config: Config,
        required_scopes: HashMap<String, OperationRequiredScopes>,
    ) -> Router {
        let mut auth_state = test_auth_state(config);
        auth_state.required_scopes = Arc::new(required_scopes);
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(from_fn_with_state(auth_state, oauth_validate))
    }

    mod oauth_validate {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

        use super::*;

        #[tokio::test]
        async fn missing_token_returns_unauthorized() {
            let config = test_config();
            let app = test_router(config.clone());
            let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            let headers = res.headers();
            let www_auth = headers.get(WWW_AUTHENTICATE).unwrap().to_str().unwrap();
            assert!(www_auth.contains("Bearer"));
            assert!(www_auth.contains("resource_metadata"));
        }

        #[tokio::test]
        async fn invalid_token_returns_unauthorized() {
            let config = test_config();
            let app = test_router(config.clone());
            let req = Request::builder()
                .uri("/test")
                .header(AUTHORIZATION, "Bearer invalidtoken")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            let headers = res.headers();
            let www_auth = headers.get(WWW_AUTHENTICATE).unwrap().to_str().unwrap();
            assert!(www_auth.contains("Bearer"));
            assert!(www_auth.contains("resource_metadata"));
        }

        #[tokio::test]
        async fn missing_token_with_multiple_scopes() {
            let mut config = test_config();
            config.scopes = vec!["read".to_string(), "write".to_string()];
            let app = test_router(config);
            let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            let headers = res.headers();
            let www_auth = headers.get(WWW_AUTHENTICATE).unwrap().to_str().unwrap();
            assert!(www_auth.contains(r#"scope="read write""#));
        }

        #[tokio::test]
        async fn missing_token_without_scopes_omits_scope_parameter() {
            let mut config = test_config();
            config.scopes = vec![];
            let app = test_router(config);
            let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            let headers = res.headers();
            let www_auth = headers.get(WWW_AUTHENTICATE).unwrap().to_str().unwrap();
            assert!(www_auth.contains("Bearer"));
            assert!(www_auth.contains("resource_metadata"));
            assert!(!www_auth.contains("scope="));
        }

        async fn valid_token_with_insufficient_scopes_response() -> (StatusCode, String) {
            let mut server = mockito::Server::new_async().await;
            let kid = "test-kid";
            let secret = b"hs512-integration-test-signing-secret";

            let discovery = format!(
                r#"{{"issuer":"{url}","jwks_uri":"{url}/jwks","id_token_signing_alg_values_supported":["HS512"]}}"#,
                url = server.url()
            );
            let _discovery = server
                .mock("GET", "/.well-known/oauth-authorization-server")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(discovery)
                .create_async()
                .await;

            // Symmetric (`oct`) JWK whose secret matches the signing key below.
            let jwks = format!(
                r#"{{"keys":[{{"kty":"oct","alg":"HS512","use":"sig","kid":"{kid}","k":"{k}"}}]}}"#,
                k = URL_SAFE_NO_PAD.encode(secret)
            );
            let _jwks = server
                .mock("GET", "/jwks")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(jwks)
                .create_async()
                .await;

            // A genuinely valid token that carries `read` but not the required `write`.
            let exp = chrono::Utc::now().timestamp() + 1000;
            let claims = serde_json::json!({
                "aud": "test-audience",
                "exp": exp,
                "sub": "test-user",
                "scope": "read",
            });
            let header = {
                let mut h = Header::new(Algorithm::HS512);
                h.kid = Some(kid.to_string());
                h
            };
            let token =
                encode(&header, &claims, &EncodingKey::from_secret(secret)).expect("encode JWT");

            let mut config = test_config();
            config.servers = vec![server.url()];
            config.scopes = vec!["write".to_string()];
            let app = test_router(config);

            let req = Request::builder()
                .uri("/test")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();

            let status = res.status();
            let www_auth = res
                .headers()
                .get(WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap();

            (status, www_auth.to_string())
        }

        #[tokio::test]
        async fn valid_token_with_insufficient_scopes_returns_forbidden() {
            let (status, _) = valid_token_with_insufficient_scopes_response().await;

            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn valid_token_with_insufficient_scopes_sets_insufficient_scope_error() {
            let (_, www_auth) = valid_token_with_insufficient_scopes_response().await;

            assert!(
                www_auth.contains(r#"error="insufficient_scope""#),
                "got: {www_auth}"
            );
        }

        #[tokio::test]
        async fn valid_token_with_insufficient_scopes_includes_required_scope() {
            let (_, www_auth) = valid_token_with_insufficient_scopes_response().await;

            assert!(www_auth.contains(r#"scope="write""#), "got: {www_auth}");
        }

        /// Drives a real `tools/call` request through the middleware for an
        /// operation with a nested (OR-of-AND) scope requirement, where the
        /// token satisfies neither group completely but is closer to the
        /// second: the first group is missing both scopes, the second only
        /// one. The global `scope_mode` is `require_any`, distinct from the
        /// per-operation challenge's `require_all`.
        async fn insufficient_nested_scope_response() -> (StatusCode, String) {
            let mut server = mockito::Server::new_async().await;
            let kid = "test-kid";
            let secret = b"hs512-integration-test-signing-secret";

            let discovery = format!(
                r#"{{"issuer":"{url}","jwks_uri":"{url}/jwks","id_token_signing_alg_values_supported":["HS512"]}}"#,
                url = server.url()
            );
            let _discovery = server
                .mock("GET", "/.well-known/oauth-authorization-server")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(discovery)
                .create_async()
                .await;

            let jwks = format!(
                r#"{{"keys":[{{"kty":"oct","alg":"HS512","use":"sig","kid":"{kid}","k":"{k}"}}]}}"#,
                k = URL_SAFE_NO_PAD.encode(secret)
            );
            let _jwks = server
                .mock("GET", "/jwks")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(jwks)
                .create_async()
                .await;

            // Satisfies the global `read` scope (test_config's default) and
            // one scope of the second operation-level group, but not enough
            // to complete either group.
            let exp = chrono::Utc::now().timestamp() + 1000;
            let claims = serde_json::json!({
                "aud": "test-audience",
                "exp": exp,
                "sub": "test-user",
                "scope": "read admin",
            });
            let header = {
                let mut h = Header::new(Algorithm::HS512);
                h.kid = Some(kid.to_string());
                h
            };
            let token =
                encode(&header, &claims, &EncodingKey::from_secret(secret)).expect("encode JWT");

            let mut config = test_config();
            config.servers = vec![server.url()];
            config.scope_mode = ScopeMode::RequireAny;
            let required_scopes = HashMap::from([(
                "RestrictedOp".to_string(),
                OperationRequiredScopes::new(vec![
                    vec!["sensitive:read".to_string(), "tenant:admin".to_string()],
                    vec!["admin".to_string(), "superuser".to_string()],
                ])
                .expect("valid multi-group requirement"),
            )]);
            let app = test_router_with_required_scopes(config, required_scopes);

            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "RestrictedOp"}
            })
            .to_string();

            let req = Request::builder()
                .method("POST")
                .uri("/test")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let res = app.oneshot(req).await.unwrap();

            let status = res.status();
            let www_auth = res
                .headers()
                .get(WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();

            (status, www_auth)
        }

        #[tokio::test]
        async fn insufficient_nested_scope_returns_forbidden() {
            let (status, _) = insufficient_nested_scope_response().await;

            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn insufficient_nested_scope_challenge_names_the_first_listed_alternative() {
            let (_, www_auth) = insufficient_nested_scope_response().await;

            // The token is closer to satisfying the second group, but the
            // challenge always names the first-listed alternative regardless.
            assert!(
                www_auth.contains(r#"scope="sensitive:read tenant:admin""#),
                "got: {www_auth}"
            );
        }

        #[tokio::test]
        async fn insufficient_nested_scope_challenge_reports_require_all() {
            let (_, www_auth) = insufficient_nested_scope_response().await;

            // The global scope_mode is require_any, but a per-operation
            // challenge always reports require_all: every scope in the
            // advertised group is required.
            assert!(
                www_auth.contains(r#"scope_mode="require_all""#),
                "got: {www_auth}"
            );
        }
    }

    mod scope_mode {
        use super::*;

        #[test]
        fn as_str_matches_serde_rename() {
            for mode in [
                ScopeMode::Disabled,
                ScopeMode::RequireAll,
                ScopeMode::RequireAny,
            ] {
                let serde = serde_json::to_string(&mode).expect("serialize scope mode");
                assert_eq!(
                    format!("\"{}\"", mode.as_str()),
                    serde,
                    "as_str drifted from the serde rename for {mode:?}"
                );
            }
        }
    }

    mod scope_validation {
        use super::*;
        use rstest::rstest;

        fn is_sufficient(mode: ScopeMode, required: &[String], present: &[String]) -> bool {
            required.is_empty() || mode.is_satisfied_by(required, present)
        }

        fn s(vals: &[&str]) -> Vec<String> {
            vals.iter().map(|v| v.to_string()).collect()
        }

        #[rstest]
        #[case::all_present(ScopeMode::RequireAll, &["read", "write"], &["read", "write"], true)]
        #[case::missing_one(ScopeMode::RequireAll, &["read", "write"], &["read"], false)]
        #[case::none_present(ScopeMode::RequireAll, &["read"], &[], false)]
        #[case::superset(ScopeMode::RequireAll, &["read"], &["read", "write", "admin"], true)]
        #[case::reversed_order(ScopeMode::RequireAll, &["write", "read"], &["read", "write"], true)]
        #[case::any_one_match(ScopeMode::RequireAny, &["read", "write"], &["read"], true)]
        #[case::any_zero_matches(ScopeMode::RequireAny, &["read", "write"], &["admin"], false)]
        #[case::any_none_present(ScopeMode::RequireAny, &["read"], &[], false)]
        #[case::disabled_ignores_scopes(ScopeMode::Disabled, &["read", "write"], &[], true)]
        fn scope_check(
            #[case] mode: ScopeMode,
            #[case] required: &[&str],
            #[case] present: &[&str],
            #[case] expected: bool,
        ) {
            assert_eq!(is_sufficient(mode, &s(required), &s(present)), expected);
        }

        #[rstest]
        #[case::require_all(ScopeMode::RequireAll)]
        #[case::require_any(ScopeMode::RequireAny)]
        #[case::disabled(ScopeMode::Disabled)]
        fn empty_required_scopes_is_sufficient(#[case] mode: ScopeMode) {
            assert!(is_sufficient(mode, &[], &s(&["anything"])));
        }

        #[test]
        fn forbidden_error_contains_insufficient_scope() {
            let header = WwwAuthenticate::Bearer {
                resource_metadata: Url::parse(
                    "https://test.com/.well-known/oauth-protected-resource",
                )
                .unwrap(),
                scope: Some("read write".to_string()),
                error: Some(BearerError::InsufficientScope),
                scope_mode: None,
            };

            let mut values = Vec::new();
            headers::Header::encode(&header, &mut values);
            let encoded = values.first().unwrap().to_str().unwrap();

            assert!(encoded.contains(r#"error="insufficient_scope""#));
        }

        #[test]
        fn forbidden_error_includes_required_scopes() {
            let header = WwwAuthenticate::Bearer {
                resource_metadata: Url::parse(
                    "https://test.com/.well-known/oauth-protected-resource",
                )
                .unwrap(),
                scope: Some("read write".to_string()),
                error: Some(BearerError::InsufficientScope),
                scope_mode: None,
            };

            let mut values = Vec::new();
            headers::Header::encode(&header, &mut values);
            let encoded = values.first().unwrap().to_str().unwrap();

            assert!(encoded.contains(r#"scope="read write""#));
        }

        #[test]
        fn scope_mode_yaml_deserialization() {
            let yaml = r#"
                servers:
                  - http://localhost:1234
                audiences:
                  - test-audience
                resource: http://localhost:4000
                scopes:
                  - read
                scope_mode: require_any
            "#;

            let config: Config = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(config.scope_mode, ScopeMode::RequireAny);
        }

        #[test]
        fn scope_mode_defaults_to_require_all() {
            let yaml = r#"
                servers:
                  - http://localhost:1234
                audiences:
                  - test-audience
                resource: http://localhost:4000
                scopes:
                  - read
            "#;

            let config: Config = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(config.scope_mode, ScopeMode::RequireAll);
        }
    }

    mod tls_config {
        use super::*;
        use std::io::Write;
        use tempfile::NamedTempFile;

        #[test]
        fn rejects_server_url_without_host() {
            let mut config = test_config();
            // file:// URLs have no host
            config.servers = vec!["file:///some/path".to_string()];

            let router = Router::new();
            let err = config
                .enable_middleware(router, HashMap::new())
                .unwrap_err();

            assert!(matches!(
                err,
                TlsConfigError::ServerUrlMissingHost { index: 0, .. }
            ));
        }

        #[test]
        fn default_config_builds_client() {
            let config = TlsConfig::default();
            let client = config.build_client(HeaderMap::new());
            assert!(client.is_ok());
        }

        #[test]
        fn danger_accept_invalid_certs_builds_client() {
            let config = TlsConfig {
                ca_cert: None,
                danger_accept_invalid_certs: true,
            };
            let client = config.build_client(HeaderMap::new());
            assert!(client.is_ok());
        }

        #[test]
        fn valid_ca_cert_is_loaded() {
            // Create a temporary file with a valid PEM certificate
            // This is the ISRG Root X1 certificate (Let's Encrypt root CA)
            let mut temp_file = NamedTempFile::new().unwrap();
            let test_cert = r#"-----BEGIN CERTIFICATE-----
MIIFazCCA1OgAwIBAgIRAIIQz7DSQONZRGPgu2OCiwAwDQYJKoZIhvcNAQELBQAw
TzELMAkGA1UEBhMCVVMxKTAnBgNVBAoTIEludGVybmV0IFNlY3VyaXR5IFJlc2Vh
cmNoIEdyb3VwMRUwEwYDVQQDEwxJU1JHIFJvb3QgWDEwHhcNMTUwNjA0MTEwNDM4
WhcNMzUwNjA0MTEwNDM4WjBPMQswCQYDVQQGEwJVUzEpMCcGA1UEChMgSW50ZXJu
ZXQgU2VjdXJpdHkgUmVzZWFyY2ggR3JvdXAxFTATBgNVBAMTDElTUkcgUm9vdCBY
MTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBAK3oJHP0FDfzm54rVygc
h77ct984kIxuPOZXoHj3dcKi/vVqbvYATyjb3miGbESTtrFj/RQSa78f0uoxmyF+
0TM8ukj13Xnfs7j/EvEhmkvBioZxaUpmZmyPfjxwv60pIgbz5MDmgK7iS4+3mX6U
A5/TR5d8mUgjU+g4rk8Kb4Mu0UlXjIB0ttov0DiNewNwIRt18jA8+o+u3dpjq+sW
T8KOEUt+zwvo/7V3LvSye0rgTBIlDHCNAymg4VMk7BPZ7hm/ELNKjD+Jo2FR3qyH
B5T0Y3HsLuJvW5iB4YlcNHlsdu87kGJ55tukmi8mxdAQ4Q7e2RCOFvu396j3x+UC
B5iPNgiV5+I3lg02dZ77DnKxHZu8A/lJBdiB3QW0KtZB6awBdpUKD9jf1b0SHzUv
KBds0pjBqAlkd25HN7rOrFleaJ1/ctaJxQZBKT5ZPt0m9STJEadao0xAH0ahmbWn
OlFuhjuefXKnEgV4We0+UXgVCwOPjdAvBbI+e0ocS3MFEvzG6uBQE3xDk3SzynTn
jh8BCNAw1FtxNrQHusEwMFxIt4I7mKZ9YIqioymCzLq9gwQbooMDQaHWBfEbwrbw
qHyGO0aoSCqI3Haadr8faqU9GY/rOPNk3sgrDQoo//fb4hVC1CLQJ13hef4Y53CI
rU7m2Ys6xt0nUW7/vGT1M0NPAgMBAAGjQjBAMA4GA1UdDwEB/wQEAwIBBjAPBgNV
HRMBAf8EBTADAQH/MB0GA1UdDgQWBBR5tFnme7bl5AFzgAiIyBpY9umbbjANBgkq
hkiG9w0BAQsFAAOCAgEAVR9YqbyyqFDQDLHYGmkgJykIrGF1XIpu+ILlaS/V9lZL
ubhzEFnTIZd+50xx+7LSYK05qAvqFyFWhfFQDlnrzuBZ6brJFe+GnY+EgPbk6ZGQ
3BebYhtF8GaV0nxvwuo77x/Py9auJ/GpsMiu/X1+mvoiBOv/2X/qkSsisRcOj/KK
NFtY2PwByVS5uCbMiogziUwthDyC3+6WVwW6LLv3xLfHTjuCvjHIInNzktHCgKQ5
ORAzI4JMPJ+GslWYHb4phowim57iaztXOoJwTdwJx4nLCgdNbOhdjsnvzqvHu7Ur
TkXWStAmzOVyyghqpZXjFaH3pO3JLF+l+/+sKAIuvtd7u+Nxe5AW0wdeRlN8NwdC
jNPElpzVmbUq4JUagEiuTDkHzsxHpFKVK7q4+63SM1N95R1NbdWhscdCb+ZAJzVc
oyi3B43njTOQ5yOf+1CceWxG1bQVs5ZufpsMljq4Ui0/1lvh+wjChP4kqKOJ2qxq
4RgqsahDYVvTH9w7jXbyLeiNdd8XM2w9U/t7y0Ff/9yi0GE44Za4rF2LN9d11TPA
mRGunUHBcnWEvgJBQl9nJEiU0Zsnvgc/ubhPgXRR4Xq37Z0j4r7g1SgEEzwxA57d
emyPxgcYxn/eR44/KJ4EBs+lVDR3veyJm+kXQ99b21/+jh5Xos1AnX5iItreGCc=
-----END CERTIFICATE-----"#;
            temp_file.write_all(test_cert.as_bytes()).unwrap();

            let config = TlsConfig {
                ca_cert: Some(temp_file.path().to_path_buf()),
                danger_accept_invalid_certs: false,
            };
            let client = config.build_client(HeaderMap::new());
            assert!(client.is_ok());
        }

        #[test]
        fn missing_ca_cert_file_returns_error() {
            let config = TlsConfig {
                ca_cert: Some("/nonexistent/path/to/cert.pem".into()),
                danger_accept_invalid_certs: false,
            };
            let result = config.build_client(HeaderMap::new());
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                TlsConfigError::CertificateRead { .. }
            ));
        }

        #[test]
        fn invalid_pem_returns_error() {
            // Create a temporary file with invalid PEM content
            let mut temp_file = NamedTempFile::new().unwrap();
            temp_file.write_all(b"not a valid certificate").unwrap();

            let config = TlsConfig {
                ca_cert: Some(temp_file.path().to_path_buf()),
                danger_accept_invalid_certs: false,
            };
            let result = config.build_client(HeaderMap::new());
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                TlsConfigError::CertificateParse { .. }
            ));
        }

        #[test]
        fn yaml_deserialization_with_discovery_timeout() {
            let y = r#"
              servers:
                - http://localhost:1234
              audiences:
                - test-audience
              resource: http://localhost:4000
              scopes:
                - read
              discovery_timeout: 10s
            "#;

            let config: Config = serde_yaml::from_str(y).unwrap();
            assert_eq!(config.discovery_timeout, Some(Duration::from_secs(10)));
        }

        #[test]
        fn yaml_deserialization_without_discovery_timeout_defaults_to_none() {
            let y = r#"
              servers:
                - http://localhost:1234
              audiences:
                - test-audience
              resource: http://localhost:4000
              scopes:
                - read
            "#;

            let config: Config = serde_yaml::from_str(y).unwrap();
            assert_eq!(config.discovery_timeout, None);
        }

        #[test]
        fn yaml_deserialization_with_discovery_headers() {
            let y = r#"
              servers:
                - http://localhost:1234
              audiences:
                - test-audience
              resource: http://localhost:4000
              scopes:
                - read
              discovery_headers:
                User-Agent: apollo-mcp-server
                X-Custom-Header: custom-value
            "#;

            let config: Config = serde_yaml::from_str(y).unwrap();
            assert_eq!(
                config
                    .discovery_headers
                    .get("user-agent")
                    .map(|v| v.to_str().unwrap()),
                Some("apollo-mcp-server")
            );
            assert_eq!(
                config
                    .discovery_headers
                    .get("x-custom-header")
                    .map(|v| v.to_str().unwrap()),
                Some("custom-value")
            );
        }

        #[test]
        fn yaml_deserialization_without_discovery_headers_defaults_to_empty() {
            let y = r#"
              servers:
                - http://localhost:1234
              audiences:
                - test-audience
              resource: http://localhost:4000
              scopes:
                - read
            "#;

            let config: Config = serde_yaml::from_str(y).unwrap();
            assert!(config.discovery_headers.is_empty());
        }

        #[tokio::test]
        async fn build_client_with_discovery_headers() {
            let mut mock_server = mockito::Server::new_async().await;
            let mock = mock_server
                .mock("GET", "/test")
                .match_header("user-agent", "apollo-mcp-server")
                .create_async()
                .await;

            let config = TlsConfig::default();
            let mut headers = HeaderMap::new();
            headers.insert("user-agent", HeaderValue::from_static("apollo-mcp-server"));
            let client = config.build_client(headers).unwrap();

            client
                .get(format!("{}/test", mock_server.url()))
                .send()
                .await
                .unwrap();

            mock.assert_async().await;
        }
    }

    mod build_resource_metadata_url {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case::no_path(
            "https://mcp.example.com",
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        )]
        #[case::single_path_segment(
            "https://mcp.example.com/mcp",
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
        )]
        #[case::multi_path_segments(
            "https://api.example.com/first-service/mcp",
            "https://api.example.com/.well-known/oauth-protected-resource/first-service/mcp"
        )]
        #[case::trailing_slash_normalized(
            "https://mcp.example.com/mcp/",
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
        )]
        #[case::non_standard_port(
            "https://localhost:8443/mcp",
            "https://localhost:8443/.well-known/oauth-protected-resource/mcp"
        )]
        #[case::no_path_with_port(
            "https://localhost:4000",
            "https://localhost:4000/.well-known/oauth-protected-resource"
        )]
        #[case::deep_path(
            "https://api.example.com/v1/services/mcp",
            "https://api.example.com/.well-known/oauth-protected-resource/v1/services/mcp"
        )]
        #[case::query_string_stripped(
            "https://mcp.example.com/mcp?version=2",
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
        )]
        #[case::fragment_stripped(
            "https://mcp.example.com/mcp#section",
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
        )]
        #[case::root_trailing_slash(
            "https://mcp.example.com/",
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        )]
        fn constructs_correct_url(#[case] resource: &str, #[case] expected: &str) {
            let resource_url = Url::parse(resource).unwrap();
            let result = build_resource_metadata_url(&resource_url);
            assert_eq!(result.as_str(), expected);
        }
    }

    mod servers_field {
        use super::*;

        #[test]
        fn rejects_unparseable_server_url_at_load() {
            let yaml = r#"
                servers:
                  - "not a url"
                audiences:
                  - test-audience
                resource: http://localhost:4000
                scopes:
                  - read
            "#;
            let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
            assert!(
                err.to_string().contains("invalid auth server URL"),
                "got: {err}"
            );
        }
    }

    mod issuers_field {
        use super::*;

        #[test]
        fn yaml_deserialization_with_issuers() {
            let yaml = r#"
                servers:
                  - http://localhost:1234
                audiences:
                  - test-audience
                issuers:
                  - https://auth.example.com
                  - https://auth.other.com
                resource: http://localhost:4000
                scopes:
                  - read
            "#;

            let config: Config = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(
                config.issuers,
                vec![
                    "https://auth.example.com".to_string(),
                    "https://auth.other.com".to_string()
                ]
            );
        }

        #[test]
        fn yaml_deserialization_without_issuers_defaults_to_empty() {
            let yaml = r#"
                servers:
                  - http://localhost:1234
                audiences:
                  - test-audience
                resource: http://localhost:4000
                scopes:
                  - read
            "#;

            let config: Config = serde_yaml::from_str(yaml).unwrap();
            assert!(config.issuers.is_empty());
        }
    }

    mod resource_url_validation {
        use super::*;

        #[test]
        fn rejects_resource_url_with_file_scheme() {
            let mut config = test_config();
            config.resource = Url::parse("file:///some/path").unwrap();

            let err = config
                .enable_middleware(Router::new(), HashMap::new())
                .unwrap_err();

            assert!(matches!(
                err,
                TlsConfigError::ResourceUrlInvalidScheme { .. }
            ));
        }

        #[test]
        fn rejects_resource_url_with_non_http_scheme() {
            let mut config = test_config();
            config.resource = Url::parse("ftp://example.com/mcp").unwrap();

            let err = config
                .enable_middleware(Router::new(), HashMap::new())
                .unwrap_err();

            assert!(matches!(
                err,
                TlsConfigError::ResourceUrlInvalidScheme { .. }
            ));
        }

        #[test]
        fn accepts_http_resource_url() {
            let mut config = test_config();
            config.resource = Url::parse("http://localhost:4000/mcp").unwrap();

            let result = config.enable_middleware(Router::new(), HashMap::new());

            assert!(result.is_ok());
        }

        #[test]
        fn accepts_https_resource_url() {
            let mut config = test_config();
            config.resource = Url::parse("https://mcp.example.com/mcp").unwrap();

            let result = config.enable_middleware(Router::new(), HashMap::new());

            assert!(result.is_ok());
        }
    }

    mod enable_middleware_integration {
        use super::*;

        #[tokio::test]
        async fn unauthorized_response_contains_path_scoped_resource_metadata_url() {
            let mut config = test_config();
            config.resource = Url::parse("https://mcp.example.com/my-service/mcp").unwrap();

            let base_router = Router::new().route("/my-service/mcp", get(|| async { "ok" }));
            let app = config
                .enable_middleware(base_router, HashMap::new())
                .unwrap();

            let req = Request::builder()
                .uri("/my-service/mcp")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            let www_auth = res
                .headers()
                .get(WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap();
            assert!(www_auth.contains(
                r#"resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/my-service/mcp""#
            ));
        }

        #[tokio::test]
        async fn get_resource_metadata_path_returns_ok() {
            let mut config = test_config();
            config.resource = Url::parse("https://mcp.example.com/my-service/mcp").unwrap();

            let base_router = Router::new().route("/my-service/mcp", get(|| async { "ok" }));
            let app = config
                .enable_middleware(base_router, HashMap::new())
                .unwrap();

            let req = Request::builder()
                .uri("/.well-known/oauth-protected-resource/my-service/mcp")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn root_resource_metadata_path_returns_ok() {
            let config = test_config();

            let base_router = Router::new().route("/mcp", get(|| async { "ok" }));
            let app = config
                .enable_middleware(base_router, HashMap::new())
                .unwrap();

            let req = Request::builder()
                .uri("/.well-known/oauth-protected-resource")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }
    }

    mod anonymous_mcp_discovery {
        #![allow(deprecated)]
        use super::*;
        use axum::routing::post;
        use tracing_test::traced_test;

        fn discovery_router(allow: bool) -> Router {
            let mut config = test_config();
            config.allow_anonymous_mcp_discovery = allow;
            Router::new()
                .route("/mcp", post(|| async { "ok" }))
                .layer(from_fn_with_state(test_auth_state(config), oauth_validate))
        }

        fn tools_list_body() -> Body {
            Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        }

        fn tools_call_body() -> Body {
            Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test"}}"#)
        }

        fn initialize_body() -> Body {
            Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
        }

        fn resources_list_body() -> Body {
            Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#)
        }

        #[tokio::test]
        async fn initialize_without_token_allowed_when_enabled() {
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(initialize_body())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn resources_list_without_token_allowed_when_enabled() {
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(resources_list_body())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn tools_list_without_token_allowed_when_enabled() {
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(tools_list_body())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn tools_list_without_token_rejected_when_disabled() {
            let app = discovery_router(false);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(tools_list_body())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn tools_call_without_token_rejected_when_enabled() {
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(tools_call_body())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn get_without_token_rejected_when_enabled() {
            // Anonymous discovery bypass only applies to POST requests;
            // GET requests are rejected by the auth middleware before routing.
            let app = discovery_router(true);
            let req = Request::builder()
                .method("GET")
                .uri("/mcp")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn malformed_json_without_token_falls_through_to_unauthorized() {
            // A body that fails to parse also fails to match a skip list; the
            // tokenless caller gets the normal 401 challenge, not an opaque 400.
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(Body::from("not json"))
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn empty_body_without_token_falls_through_to_unauthorized() {
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn oversized_body_without_token_falls_through_to_unauthorized() {
            // A body too large to peek simply fails to match a skip list; without
            // a token the request still ends at the normal 401 challenge, not a
            // 413 that gives the caller no actionable next step.
            let app = discovery_router(true);
            let body = Body::from("x".repeat(super::PEEK_BODY_LIMIT + 1));
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(body)
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        #[traced_test]
        async fn oversized_body_falling_through_logs_distinctly_from_a_plain_401() {
            // The 401 above is indistinguishable from one caused by a body too
            // large to peek unless this case is logged on its own.
            let app = discovery_router(true);
            let body = Body::from("x".repeat(super::PEEK_BODY_LIMIT + 1));
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(body)
                .unwrap();
            let _res = app.oneshot(req).await.unwrap();
            assert!(logs_contain("falling through to normal token validation"));
        }

        #[tokio::test]
        async fn oversized_body_with_a_bad_token_is_rejected_for_the_token_not_the_body() {
            let app = discovery_router(true);
            let body = Body::from("x".repeat(super::PEEK_BODY_LIMIT + 1));
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(AUTHORIZATION, "Bearer expired-or-malformed")
                .body(body)
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn tools_list_with_invalid_token_still_validates_when_enabled() {
            let app = discovery_router(true);
            let req = Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(AUTHORIZATION, "Bearer invalidtoken")
                .body(tools_list_body())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        #[test]
        fn yaml_with_allow_anonymous_mcp_discovery() {
            let yaml = r#"
                servers:
                  - http://localhost:1234
                audiences:
                  - test-audience
                resource: http://localhost:4000
                scopes:
                  - read
                allow_anonymous_mcp_discovery: true
            "#;
            let config: Config = serde_yaml::from_str(yaml).unwrap();
            assert!(config.allow_anonymous_mcp_discovery);
        }

        #[test]
        fn yaml_defaults_allow_anonymous_mcp_discovery_to_false() {
            let yaml = r#"
                servers:
                  - http://localhost:1234
                audiences:
                  - test-audience
                resource: http://localhost:4000
                scopes:
                  - read
            "#;
            let config: Config = serde_yaml::from_str(yaml).unwrap();
            assert!(!config.allow_anonymous_mcp_discovery);
        }
    }

    mod skip_token_validation {
        use super::*;
        use crate::scope_requirements::OperationRequiredScopes;
        use axum::routing::post;

        fn skip(methods: &[&str], tools: &[&str], headers: &[&str]) -> SkipTokenValidation {
            SkipTokenValidation {
                methods: methods.iter().map(|m| (*m).to_string()).collect(),
                tools: tools.iter().map(|t| (*t).to_string()).collect(),
                headers: headers
                    .iter()
                    .map(|h| HeaderName::from_bytes(h.as_bytes()).expect("valid test header name"))
                    .collect(),
            }
        }

        fn skip_router(lists: SkipTokenValidation) -> Router {
            let mut config = test_config();
            config.skip_token_validation = lists;
            Router::new()
                .route("/mcp", post(|| async { "ok" }).get(|| async { "ok" }))
                .layer(from_fn_with_state(test_auth_state(config), oauth_validate))
        }

        fn skip_router_with_required_scopes(
            lists: SkipTokenValidation,
            required_scopes: HashMap<String, OperationRequiredScopes>,
        ) -> Router {
            let mut config = test_config();
            config.skip_token_validation = lists;
            let mut auth_state = test_auth_state(config);
            auth_state.required_scopes = Arc::new(required_scopes);
            Router::new()
                .route("/mcp", post(|| async { "ok" }).get(|| async { "ok" }))
                .layer(from_fn_with_state(auth_state, oauth_validate))
        }

        fn method_body(method: &str) -> Body {
            Body::from(format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#))
        }

        fn tool_call_body(tool: &str) -> Body {
            Body::from(format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}"}}}}"#
            ))
        }

        fn post_request(body: Body) -> Request<Body> {
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(body)
                .unwrap()
        }

        mod method_list {
            use super::*;

            #[tokio::test]
            async fn listed_method_without_token_passes() {
                let app = skip_router(skip(&["server/discover"], &[], &[]));
                let res = app
                    .oneshot(post_request(method_body("server/discover")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }

            #[tokio::test]
            async fn unlisted_method_without_token_is_rejected() {
                let app = skip_router(skip(&["server/discover"], &[], &[]));
                let res = app
                    .oneshot(post_request(method_body("resources/list")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn listed_method_with_a_bad_token_is_still_rejected() {
                // The lists never rescue a token the caller chose to present.
                let app = skip_router(skip(&["server/discover"], &[], &[]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(AUTHORIZATION, "Bearer expired-or-malformed")
                    .body(method_body("server/discover"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn an_empty_list_lets_nothing_through() {
                let app = skip_router(SkipTokenValidation::default());
                let res = app
                    .oneshot(post_request(method_body("tools/list")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }
        }

        mod tool_list {
            use super::*;

            #[tokio::test]
            async fn listed_tool_without_token_passes() {
                let app = skip_router(skip(&[], &["ApolloDocsSearch"], &[]));
                let res = app
                    .oneshot(post_request(tool_call_body("ApolloDocsSearch")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }

            #[tokio::test]
            async fn unlisted_tool_without_token_is_rejected() {
                let app = skip_router(skip(&[], &["ApolloDocsSearch"], &[]));
                let res = app
                    .oneshot(post_request(tool_call_body("GetVariantDetails")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn listed_tool_with_a_bad_token_is_still_rejected() {
                let app = skip_router(skip(&[], &["ApolloDocsSearch"], &[]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(AUTHORIZATION, "Bearer expired-or-malformed")
                    .body(tool_call_body("ApolloDocsSearch"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn a_tool_call_with_no_name_is_rejected() {
                let app = skip_router(skip(&[], &["ApolloDocsSearch"], &[]));
                let res = app
                    .oneshot(post_request(method_body("tools/call")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn an_app_qualified_call_to_a_listed_tool_is_rejected() {
                // `?app=` makes the dispatcher run the app's own tool, so the
                // name in `tools` no longer identifies what would execute.
                let app = skip_router(skip(&[], &["ApolloDocsSearch"], &[]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp?app=SomeApp")
                    .body(tool_call_body("ApolloDocsSearch"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn an_unrelated_query_parameter_does_not_block_a_listed_tool() {
                let app = skip_router(skip(&[], &["ApolloDocsSearch"], &[]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp?trace=1")
                    .body(tool_call_body("ApolloDocsSearch"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }

            #[tokio::test]
            async fn an_app_qualified_listed_method_still_passes() {
                // Only the tool half keys on an identity `?app=` can change.
                let app = skip_router(skip(&["tools/list"], &[], &[]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp?app=SomeApp")
                    .body(method_body("tools/list"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }
        }

        mod header_list {
            use super::*;

            #[tokio::test]
            async fn listed_header_without_token_passes() {
                let app = skip_router(skip(&[], &[], &["x-api-key"]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("x-api-key", "service:graph:secret")
                    .body(tool_call_body("GetVariantDetails"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }

            #[tokio::test]
            async fn a_request_without_the_header_is_rejected() {
                let app = skip_router(skip(&[], &[], &["x-api-key"]));
                let res = app
                    .oneshot(post_request(tool_call_body("GetVariantDetails")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn listed_header_with_a_bad_token_is_still_rejected() {
                let app = skip_router(skip(&[], &[], &["x-api-key"]));
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(AUTHORIZATION, "Bearer expired-or-malformed")
                    .header("x-api-key", "service:graph:secret")
                    .body(tool_call_body("GetVariantDetails"))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            }

            #[tokio::test]
            async fn listed_header_passes_on_a_request_with_no_json_rpc_body() {
                // The header list reads no body, so it also covers GET, which
                // carries the server-to-client stream.
                let app = skip_router(skip(&[], &[], &["x-api-key"]));
                let req = Request::builder()
                    .method("GET")
                    .uri("/mcp")
                    .header("x-api-key", "service:graph:secret")
                    .body(Body::empty())
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }
        }

        mod regression {
            use super::*;

            #[tokio::test]
            async fn an_unmatched_request_still_gets_the_bearer_challenge() {
                let app = skip_router(skip(&["tools/list"], &["ApolloDocsSearch"], &["x-api-key"]));
                let res = app
                    .oneshot(post_request(tool_call_body("GetVariantDetails")))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
                let www_auth = res
                    .headers()
                    .get(WWW_AUTHENTICATE)
                    .unwrap()
                    .to_str()
                    .unwrap();
                assert!(www_auth.contains("Bearer"));
                assert!(www_auth.contains("resource_metadata"));
            }
        }

        mod config_validation {
            use super::*;

            fn parse(extra: &str) -> Result<Config, serde_yaml::Error> {
                serde_yaml::from_str(&format!(
                    "servers:\n  \
                     - http://localhost:1234\n\
                     resource: http://localhost:4000\n\
                     scopes:\n  \
                     - read\n\
                     {extra}"
                ))
            }

            /// The three lists together, so each test below reads one of them
            /// out of the same document.
            fn all_lists() -> Config {
                parse(
                    "skip_token_validation:\n  \
                     methods:\n    \
                     - tools/list\n  \
                     tools:\n    \
                     - ApolloDocsSearch\n  \
                     headers:\n    \
                     - x-api-key\n",
                )
                .expect("valid test config")
            }

            #[test]
            fn the_method_list_parses() {
                assert_eq!(
                    all_lists().skip_token_validation.methods,
                    vec!["tools/list"]
                );
            }

            #[test]
            fn the_tool_list_parses() {
                assert_eq!(
                    all_lists().skip_token_validation.tools,
                    vec!["ApolloDocsSearch"]
                );
            }

            #[test]
            fn the_header_list_parses() {
                assert_eq!(
                    all_lists().skip_token_validation.headers,
                    vec![HeaderName::from_static("x-api-key")]
                );
            }

            #[test]
            fn the_lists_default_to_empty() {
                let lists = parse("").unwrap().skip_token_validation;
                assert!(
                    !lists.needs_body() && lists.headers.is_empty(),
                    "expected every list empty, found {lists:?}"
                );
            }

            #[test]
            fn tools_call_is_rejected_in_the_method_list() {
                let err = parse("skip_token_validation:\n  methods:\n    - tools/call\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("skip_token_validation.tools"), "{err}");
            }

            #[test]
            fn resources_read_is_rejected_in_the_method_list() {
                let err = parse("skip_token_validation:\n  methods:\n    - resources/read\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("resources/read"), "{err}");
            }

            #[test]
            fn prompts_get_is_rejected_in_the_method_list() {
                let err = parse("skip_token_validation:\n  methods:\n    - prompts/get\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("prompts/get"), "{err}");
            }

            #[test]
            fn resources_subscribe_is_rejected_in_the_method_list() {
                let err = parse("skip_token_validation:\n  methods:\n    - resources/subscribe\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("resources/subscribe"), "{err}");
            }

            #[test]
            fn resources_unsubscribe_is_rejected_in_the_method_list() {
                let err =
                    parse("skip_token_validation:\n  methods:\n    - resources/unsubscribe\n")
                        .unwrap_err()
                        .to_string();
                assert!(err.contains("resources/unsubscribe"), "{err}");
            }

            #[test]
            fn completion_complete_is_rejected_in_the_method_list() {
                let err = parse("skip_token_validation:\n  methods:\n    - completion/complete\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("completion/complete"), "{err}");
            }

            #[test]
            fn an_invalid_header_name_is_rejected() {
                let err = parse("skip_token_validation:\n  headers:\n    - \"not a header\"\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("invalid header name"), "{err}");
            }

            #[test]
            fn authorization_is_rejected_in_the_header_list() {
                let err = parse("skip_token_validation:\n  headers:\n    - authorization\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("could never match"), "{err}");
            }

            #[test]
            fn authorization_is_rejected_regardless_of_casing() {
                let err = parse("skip_token_validation:\n  headers:\n    - Authorization\n")
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("could never match"), "{err}");
            }
        }

        mod deprecated_flag {
            #![allow(deprecated)]
            use super::*;

            #[test]
            fn it_maps_onto_the_method_list() {
                let mut config = test_config();
                config.allow_anonymous_mcp_discovery = true;
                let resolved = config.resolve_skip_token_validation().unwrap();
                assert_eq!(
                    resolved.methods,
                    vec!["initialize", "tools/list", "resources/list"]
                );
            }

            #[test]
            fn it_keeps_the_tool_list() {
                let mut config = test_config();
                config.allow_anonymous_mcp_discovery = true;
                config.skip_token_validation = skip(&[], &["ApolloDocsSearch"], &["x-api-key"]);
                let resolved = config.resolve_skip_token_validation().unwrap();
                assert_eq!(resolved.tools, vec!["ApolloDocsSearch"]);
            }

            #[test]
            fn it_keeps_the_header_list() {
                let mut config = test_config();
                config.allow_anonymous_mcp_discovery = true;
                config.skip_token_validation = skip(&[], &["ApolloDocsSearch"], &["x-api-key"]);
                let resolved = config.resolve_skip_token_validation().unwrap();
                assert_eq!(resolved.headers, vec![HeaderName::from_static("x-api-key")]);
            }

            #[test]
            fn setting_it_alongside_a_method_list_is_an_error() {
                let mut config = test_config();
                config.allow_anonymous_mcp_discovery = true;
                config.skip_token_validation = skip(&["server/discover"], &[], &[]);
                let result = config.resolve_skip_token_validation();
                assert!(
                    matches!(result, Err(TlsConfigError::AnonymousDiscoveryConflict)),
                    "expected AnonymousDiscoveryConflict, found {:?}",
                    result.map(|lists| lists.methods)
                );
            }

            #[test]
            fn setting_it_alongside_a_method_list_fails_startup() {
                let mut config = test_config();
                config.allow_anonymous_mcp_discovery = true;
                config.skip_token_validation = skip(&["server/discover"], &[], &[]);

                let result = config.enable_middleware(Router::new(), HashMap::new());

                assert!(
                    matches!(result, Err(TlsConfigError::AnonymousDiscoveryConflict)),
                    "expected enable_middleware to surface the conflict at startup, found {:?}",
                    result.map(|_| ())
                );
            }

            #[test]
            fn the_conflict_error_names_both_settings() {
                let mut config = test_config();
                config.allow_anonymous_mcp_discovery = true;
                config.skip_token_validation = skip(&["server/discover"], &[], &[]);
                let message = config
                    .resolve_skip_token_validation()
                    .unwrap_err()
                    .to_string();
                assert!(
                    message.contains("allow_anonymous_mcp_discovery")
                        && message.contains("skip_token_validation.methods"),
                    "operator cannot tell which settings collided: {message}"
                );
            }

            #[test]
            fn leaving_it_off_keeps_the_configured_lists() {
                let mut config = test_config();
                config.skip_token_validation = skip(&["server/discover"], &[], &[]);
                let resolved = config.resolve_skip_token_validation().unwrap();
                assert_eq!(resolved.methods, vec!["server/discover"]);
            }
        }

        mod scope_overlap_warning {
            use super::*;
            use crate::scope_requirements::OperationRequiredScopes;
            use tracing_test::traced_test;

            fn required_read_scope() -> HashMap<String, OperationRequiredScopes> {
                HashMap::from([(
                    "RestrictedOp".to_string(),
                    OperationRequiredScopes::new(vec![vec!["sensitive:read".to_string()]])
                        .expect("valid single-group requirement"),
                )])
            }

            #[test]
            #[traced_test]
            fn warns_when_a_skipped_tool_also_has_required_scopes() {
                let mut config = test_config();
                config.skip_token_validation = skip(&[], &["RestrictedOp"], &[]);

                let _app = config
                    .enable_middleware(Router::new(), required_read_scope())
                    .unwrap();

                assert!(logs_contain("RestrictedOp"));
                assert!(logs_contain("skip_token_validation.tools"));
            }

            #[test]
            #[traced_test]
            fn does_not_warn_without_overlap() {
                let mut config = test_config();
                config.skip_token_validation = skip(&[], &["PublicOp"], &[]);

                let _app = config
                    .enable_middleware(Router::new(), required_read_scope())
                    .unwrap();

                assert!(!logs_contain("skip_token_validation.tools"));
            }

            #[test]
            #[traced_test]
            fn warns_when_a_skip_header_coexists_with_any_required_scopes() {
                let mut config = test_config();
                config.skip_token_validation = skip(&[], &[], &["x-api-key"]);

                let _app = config
                    .enable_middleware(Router::new(), required_read_scope())
                    .unwrap();

                assert!(logs_contain("x-api-key"));
                assert!(logs_contain("skips token validation for every tool"));
            }

            #[test]
            #[traced_test]
            fn warns_when_a_skip_header_coexists_with_global_scopes_only() {
                // `test_config()` sets a non-empty global `scopes`, so this warns
                // even with no per-operation `required_scopes` configured.
                let mut config = test_config();
                config.skip_token_validation = skip(&[], &[], &["x-api-key"]);

                let _app = config
                    .enable_middleware(Router::new(), HashMap::new())
                    .unwrap();

                assert!(logs_contain("skips token validation for every tool"));
            }

            #[test]
            #[traced_test]
            fn does_not_warn_when_global_scope_enforcement_is_disabled() {
                // `scope_mode: Disabled` already makes every token satisfy `scopes`
                // (see `enable_middleware`'s own warning for that), so a listed
                // header voids nothing here and this warning would be noise.
                let mut config = test_config();
                config.skip_token_validation = skip(&[], &[], &["x-api-key"]);
                config.scope_mode = ScopeMode::Disabled;

                let _app = config
                    .enable_middleware(Router::new(), HashMap::new())
                    .unwrap();

                assert!(!logs_contain("skips token validation for every tool"));
            }

            #[test]
            #[traced_test]
            fn does_not_warn_for_a_skip_header_without_any_scope_requirement() {
                let mut config = test_config();
                config.skip_token_validation = skip(&[], &[], &["x-api-key"]);
                config.scopes = vec![];

                let _app = config
                    .enable_middleware(Router::new(), HashMap::new())
                    .unwrap();

                assert!(!logs_contain("skips token validation for every tool"));
            }

            #[tokio::test]
            async fn a_skip_listed_tool_actually_bypasses_its_required_scopes() {
                let app = skip_router_with_required_scopes(
                    skip(&[], &["RestrictedOp"], &[]),
                    required_read_scope(),
                );

                let res = app
                    .oneshot(post_request(tool_call_body("RestrictedOp")))
                    .await
                    .unwrap();

                assert_eq!(res.status(), StatusCode::OK);
            }

            #[tokio::test]
            async fn a_skip_header_actually_bypasses_required_scopes_for_every_tool() {
                let app = skip_router_with_required_scopes(
                    skip(&[], &[], &["x-api-key"]),
                    required_read_scope(),
                );
                let req = Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("x-api-key", "service:graph:secret")
                    .body(tool_call_body("RestrictedOp"))
                    .unwrap();

                let res = app.oneshot(req).await.unwrap();

                assert_eq!(res.status(), StatusCode::OK);
            }
        }
    }

    mod per_operation_scope_enforcement {
        use super::*;
        use crate::scope_requirements::OperationRequiredScopes;

        fn required() -> HashMap<String, OperationRequiredScopes> {
            HashMap::from([(
                "RestrictedOp".to_string(),
                OperationRequiredScopes::new(vec![vec!["sensitive:read".to_string()]])
                    .expect("valid single-group requirement"),
            )])
        }

        fn alternative_required() -> HashMap<String, OperationRequiredScopes> {
            HashMap::from([(
                "RestrictedOp".to_string(),
                OperationRequiredScopes::new(vec![
                    vec!["sensitive:read".to_string(), "tenant:admin".to_string()],
                    vec!["admin".to_string()],
                ])
                .expect("valid multi-group requirement"),
            )])
        }

        /// Two groups that each require more than one scope, so a token can
        /// touch both groups without ever completing either - unlike
        /// `alternative_required`'s single-scope `admin` group, which any
        /// presence of `admin` completes outright.
        fn overlapping_multi_scope_alternatives() -> HashMap<String, OperationRequiredScopes> {
            HashMap::from([(
                "RestrictedOp".to_string(),
                OperationRequiredScopes::new(vec![
                    vec!["scope:a".to_string(), "scope:b".to_string()],
                    vec!["scope:c".to_string(), "scope:d".to_string()],
                ])
                .expect("valid multi-group requirement"),
            )])
        }

        fn tools_call_peek(op: &str) -> JsonRpcBodyPeek {
            JsonRpcBodyPeek {
                method: "tools/call".to_string(),
                params: Some(JsonRpcParams {
                    name: Some(op.to_string()),
                }),
            }
        }

        #[test]
        fn returns_required_scopes_when_token_missing_scope() {
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec!["other:scope".to_string()];
            let required = required();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert_eq!(
                result,
                Some(
                    &OperationRequiredScopes::new(vec![vec!["sensitive:read".to_string()]])
                        .unwrap()
                )
            );
        }

        #[test]
        fn returns_none_when_token_has_required_scope() {
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec!["sensitive:read".to_string(), "other:scope".to_string()];
            let required = required();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert!(result.is_none());
        }

        #[test]
        fn returns_none_when_token_satisfies_one_scope_alternative() {
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec!["admin".to_string(), "other:scope".to_string()];
            let required = alternative_required();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert!(result.is_none());
        }

        #[test]
        fn returns_none_when_token_satisfies_one_scope_group() {
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec![
                "sensitive:read".to_string(),
                "tenant:admin".to_string(),
                "other:scope".to_string(),
            ];
            let required = alternative_required();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert!(result.is_none());
        }

        #[test]
        fn returns_required_scopes_when_token_satisfies_no_complete_alternative() {
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec!["sensitive:read".to_string()];
            let required = alternative_required();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert_eq!(
                result,
                Some(
                    &OperationRequiredScopes::new(vec![
                        vec!["sensitive:read".to_string(), "tenant:admin".to_string()],
                        vec!["admin".to_string()],
                    ])
                    .unwrap()
                )
            );
        }

        #[test]
        fn returns_required_scopes_when_scopes_are_scattered_across_groups() {
            // Touches both groups (`scope:a` from the first, `scope:c` from
            // the second) without ever completing either.
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec!["scope:a".to_string(), "scope:c".to_string()];
            let required = overlapping_multi_scope_alternatives();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert!(result.is_some());
        }

        #[test]
        fn returns_none_when_token_satisfies_more_than_one_group() {
            let peek = tools_call_peek("RestrictedOp");
            let scopes = vec![
                "scope:a".to_string(),
                "scope:b".to_string(),
                "scope:c".to_string(),
                "scope:d".to_string(),
            ];
            let required = overlapping_multi_scope_alternatives();
            let result = missing_scopes_for_operation(&peek, &required, &scopes);
            assert!(result.is_none());
        }

        #[test]
        fn returns_none_for_unrestricted_operation() {
            let peek = tools_call_peek("PublicOp");
            let required = required();
            let result = missing_scopes_for_operation(&peek, &required, &[]);
            assert!(result.is_none());
        }

        #[test]
        fn returns_none_for_non_tools_call_method() {
            let peek = JsonRpcBodyPeek {
                method: "tools/list".to_string(),
                params: None,
            };
            let required = required();
            let result = missing_scopes_for_operation(&peek, &required, &[]);
            assert!(result.is_none());
        }

        #[test]
        fn returns_none_when_required_scopes_map_is_empty() {
            let peek = tools_call_peek("RestrictedOp");
            let empty = HashMap::new();
            let result = missing_scopes_for_operation(&peek, &empty, &[]);
            assert!(result.is_none());
        }
    }
}
