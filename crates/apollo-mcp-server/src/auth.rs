use std::path::PathBuf;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    routing::get,
};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, authorization::Bearer},
};
use http::Method;
use networked_token_validator::NetworkedTokenValidator;
use schemars::JsonSchema;
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tracing::warn;
use url::Url;

mod networked_token_validator;
mod protected_resource;
mod valid_token;
mod www_authenticate;

use protected_resource::ProtectedResource;
pub(crate) use valid_token::ValidToken;
use valid_token::ValidateToken;
use www_authenticate::{BearerError, WwwAuthenticate};

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
}

impl TlsConfig {
    /// Build a reqwest client configured with the TLS settings
    pub fn build_client(&self) -> Result<reqwest::Client, TlsConfigError> {
        let mut builder = reqwest::Client::builder();

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

/// Auth configuration options
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Config {
    /// List of upstream OAuth servers to delegate auth
    pub servers: Vec<Url>,

    /// List of accepted audiences for the OAuth tokens
    #[serde(default)]
    pub audiences: Vec<String>,

    /// Allow any audience (skip validation) - use with caution
    #[serde(default)]
    pub allow_any_audience: bool,

    /// Allow clients providing their own Authorization header to bypass OAuth validation.
    ///
    /// When `true`, requests that include an `Authorization: Bearer <token>` header
    /// will skip JWT validation and pass through directly. Requests without an
    /// Authorization header will still go through the normal OAuth flow.
    ///
    /// **WARNING**: This is less secure because the server does not validate the token.
    /// Only use this when the MCP server is behind a trusted proxy or gateway that
    /// has already authenticated the client.
    #[serde(default)]
    pub allow_external_auth_header: bool,

    /// The resource to protect.
    ///
    /// Note: This is usually the publicly accessible URL of this running MCP server
    pub resource: Url,

    /// Link to documentation related to the protected resource
    pub resource_documentation: Option<Url>,

    /// Supported OAuth scopes by this resource server
    pub scopes: Vec<String>,

    /// Whether to disable the auth token passthrough to upstream API
    #[serde(default)]
    pub disable_auth_token_passthrough: bool,

    /// TLS configuration for connecting to OAuth servers
    #[serde(default)]
    pub tls: TlsConfig,

    /// Timeout for OIDC discovery requests.
    ///
    /// Accepts human-readable durations (e.g., "5s", "10s", "30s").
    /// Defaults to 5 seconds when not specified.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>")]
    pub discovery_timeout: Option<Duration>,
}

/// TLS configuration for OAuth server connections
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
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

/// Internal state for the auth middleware, containing both config and pre-built HTTP client
#[derive(Clone)]
struct AuthState {
    config: Config,
    client: reqwest::Client,
}

impl Config {
    /// Enable auth middleware on the router.
    ///
    /// Builds the HTTP client at startup to validate TLS configuration eagerly.
    pub fn enable_middleware(&self, router: Router) -> Result<Router, TlsConfigError> {
        // Validate server URLs have hosts (fail fast on config errors)
        for (i, server) in self.servers.iter().enumerate() {
            if server.host_str().is_none() {
                return Err(TlsConfigError::ServerUrlMissingHost {
                    index: i,
                    url: server.to_string(),
                });
            }
        }

        if self.allow_any_audience {
            warn!(
                "allow_any_audience is enabled - audience validation is disabled. This reduces security."
            );
        }

        if self.allow_external_auth_header {
            warn!(
                "allow_external_auth_header is enabled - requests with an Authorization header will bypass OAuth validation. \
                 This reduces security. Only use this when clients are pre-authenticated by a trusted proxy."
            );
        }

        /// Simple handler to encode our config into the desired OAuth 2.1 protected
        /// resource format
        async fn protected_resource(
            State(auth_state): State<AuthState>,
        ) -> Json<ProtectedResource> {
            Json(auth_state.config.into())
        }

        // Build HTTP client with TLS configuration
        let client = self.tls.build_client()?;
        let auth_state = AuthState {
            config: self.clone(),
            client,
        };

        // Set up auth routes. NOTE: CORs needs to allow for get requests to the
        // metadata information paths.
        let cors = CorsLayer::new()
            .allow_methods([Method::GET])
            .allow_origin(Any);
        let auth_router = Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(protected_resource),
            )
            .with_state(auth_state.clone())
            .layer(cors);

        // Merge with MCP server routes
        Ok(Router::new().merge(auth_router).merge(router.layer(
            axum::middleware::from_fn_with_state(auth_state, oauth_validate),
        )))
    }
}

/// Validate that requests made have a corresponding bearer JWT token
#[tracing::instrument(skip_all, fields(status_code, reason))]
async fn oauth_validate(
    State(auth_state): State<AuthState>,
    token: Option<TypedHeader<Authorization<Bearer>>>,
    mut request: Request,
    next: Next,
) -> Result<Response, (StatusCode, TypedHeader<WwwAuthenticate>)> {
    let auth_config = &auth_state.config;

    // Helper to construct the resource metadata URL
    let resource_metadata_url = || {
        let mut url = auth_config.resource.clone();
        url.set_path("/.well-known/oauth-protected-resource");
        url
    };

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
                resource_metadata: resource_metadata_url(),
                scope,
                error: None,
            }),
        )
    };

    // Forbidden error for valid tokens with insufficient scopes (RFC 6750 Section 3.1)
    let forbidden_error = |required_scopes: &[String]| {
        (
            StatusCode::FORBIDDEN,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource_metadata_url(),
                scope: Some(required_scopes.join(" ")),
                error: Some(BearerError::InsufficientScope),
            }),
        )
    };

    let discovery_timeout = auth_config
        .discovery_timeout
        .unwrap_or(Duration::from_secs(5));

    // If allow_external_auth_header is enabled and a token is present,
    // skip OAuth validation and pass through with the provided token.
    // No token present — fall through to normal OAuth flow (returns 401)
    if auth_config.allow_external_auth_header
        && let Some(token) = token
    {
        tracing::info!("Bypassing OAuth validation for externally-provided auth header");
        let valid_token = ValidToken {
            token: token.0,
            scopes: vec![],
        };
        request.extensions_mut().insert(valid_token);
        let response = next.run(request).await;
        tracing::Span::current().record("status_code", response.status().as_u16());
        return Ok(response);
    }

    let validator = NetworkedTokenValidator::new(
        &auth_config.audiences,
        auth_config.allow_any_audience,
        &auth_config.servers,
        &auth_state.client,
        discovery_timeout,
    );
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

    // Check if token has required scopes (fail-closed: missing scope claim = insufficient)
    if !auth_config.scopes.is_empty() {
        let missing_scopes: Vec<_> = auth_config
            .scopes
            .iter()
            .filter(|required| !valid_token.scopes.iter().any(|s| s == *required))
            .collect();

        if !missing_scopes.is_empty() {
            tracing::warn!(
                required = ?auth_config.scopes,
                present = ?valid_token.scopes,
                missing = ?missing_scopes,
                "Token has insufficient scopes"
            );
            tracing::Span::current().record("reason", "insufficient_scope");
            tracing::Span::current().record("status_code", StatusCode::FORBIDDEN.as_u16());
            return Err(forbidden_error(&auth_config.scopes));
        }
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

    fn test_config() -> Config {
        Config {
            servers: vec![Url::parse("http://localhost:1234").unwrap()],
            audiences: vec!["test-audience".to_string()],
            allow_any_audience: false,
            allow_external_auth_header: false,
            resource: Url::parse("http://localhost:4000").unwrap(),
            resource_documentation: None,
            scopes: vec!["read".to_string()],
            disable_auth_token_passthrough: false,
            tls: TlsConfig::default(),
            discovery_timeout: None,
        }
    }

    fn test_auth_state(config: Config) -> AuthState {
        AuthState {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn test_router(config: Config) -> Router {
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(from_fn_with_state(test_auth_state(config), oauth_validate))
    }

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

    #[tokio::test]
    async fn allow_external_auth_header_bypasses_validation() {
        let mut config = test_config();
        config.allow_external_auth_header = true;
        let app = test_router(config);
        let req = Request::builder()
            .uri("/test")
            .header(AUTHORIZATION, "Bearer some-external-token")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allow_external_auth_header_without_token_returns_unauthorized() {
        let mut config = test_config();
        config.allow_external_auth_header = true;
        let app = test_router(config);
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn allow_external_auth_header_disabled_rejects_invalid_token() {
        let mut config = test_config();
        config.allow_external_auth_header = false;
        let app = test_router(config);
        let req = Request::builder()
            .uri("/test")
            .header(AUTHORIZATION, "Bearer some-external-token")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    mod scope_validation {
        use super::*;

        fn scopes_are_sufficient(required: &[String], present: &[String]) -> bool {
            required.iter().all(|req| present.contains(req))
        }

        #[test]
        fn insufficient_scopes_fails() {
            let required = vec!["read".to_string(), "write".to_string()];
            let present = vec!["read".to_string()];
            assert!(!scopes_are_sufficient(&required, &present));
        }

        #[test]
        fn all_required_scopes_succeeds() {
            let required = vec!["read".to_string(), "write".to_string()];
            let present = vec!["read".to_string(), "write".to_string()];
            assert!(scopes_are_sufficient(&required, &present));
        }

        #[test]
        fn no_scopes_when_required_fails() {
            let required = vec!["read".to_string()];
            let present: Vec<String> = vec![];
            assert!(!scopes_are_sufficient(&required, &present));
        }

        #[test]
        fn superset_of_scopes_succeeds() {
            let required = vec!["read".to_string()];
            let present = vec!["read".to_string(), "write".to_string(), "admin".to_string()];
            assert!(scopes_are_sufficient(&required, &present));
        }

        #[test]
        fn empty_required_scopes_always_succeeds() {
            let required: Vec<String> = vec![];
            let present = vec!["read".to_string()];
            assert!(scopes_are_sufficient(&required, &present));

            let present_empty: Vec<String> = vec![];
            assert!(scopes_are_sufficient(&required, &present_empty));
        }

        #[test]
        fn scope_order_does_not_matter() {
            let required = vec!["write".to_string(), "read".to_string()];
            let present = vec!["read".to_string(), "write".to_string()];
            assert!(scopes_are_sufficient(&required, &present));
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
            };

            let mut values = Vec::new();
            headers::Header::encode(&header, &mut values);
            let encoded = values.first().unwrap().to_str().unwrap();

            assert!(encoded.contains(r#"error="insufficient_scope""#));
            assert!(encoded.contains(r#"scope="read write""#));
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
            config.servers = vec![Url::parse("file:///some/path").unwrap()];

            let router = Router::new();
            let result = config.enable_middleware(router);

            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                TlsConfigError::ServerUrlMissingHost { index: 0, .. }
            ));
        }

        #[test]
        fn default_config_builds_client() {
            let config = TlsConfig::default();
            let client = config.build_client();
            assert!(client.is_ok());
        }

        #[test]
        fn danger_accept_invalid_certs_builds_client() {
            let config = TlsConfig {
                ca_cert: None,
                danger_accept_invalid_certs: true,
            };
            let client = config.build_client();
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
            let client = config.build_client();
            assert!(client.is_ok());
        }

        #[test]
        fn missing_ca_cert_file_returns_error() {
            let config = TlsConfig {
                ca_cert: Some("/nonexistent/path/to/cert.pem".into()),
                danger_accept_invalid_certs: false,
            };
            let result = config.build_client();
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
            let result = config.build_client();
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
        fn yaml_deserialization_with_allow_external_auth_header() {
            let y = r#"
              servers:
                - http://localhost:1234
              audiences:
                - test-audience
              resource: http://localhost:4000
              scopes:
                - read
              allow_external_auth_header: true
            "#;

            let config: Config = serde_yaml::from_str(y).unwrap();
            assert!(config.allow_external_auth_header);
        }

        #[test]
        fn yaml_deserialization_without_allow_external_auth_header_defaults_to_false() {
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
            assert!(!config.allow_external_auth_header);
        }
    }
}
