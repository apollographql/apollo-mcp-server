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
use url::Url;

mod networked_token_validator;
mod protected_resource;
mod valid_token;
mod www_authenticate;

use protected_resource::ProtectedResource;
pub(crate) use valid_token::ValidToken;
use valid_token::ValidateToken;
use www_authenticate::WwwAuthenticate;

/// Auth configuration options
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Config {
    /// List of upstream OAuth servers to delegate auth
    pub servers: Vec<Url>,

    /// List of accepted audiences for the OAuth tokens
    pub audiences: Vec<String>,

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
}

impl Config {
    pub fn enable_middleware(&self, router: Router) -> Router {
        /// Simple handler to encode our config into the desired OAuth 2.1 protected
        /// resource format
        async fn protected_resource(State(auth_config): State<Config>) -> Json<ProtectedResource> {
            Json(auth_config.into())
        }

        // Set up auth routes. NOTE: CORs needs to allow for get requests to the
        // metadata information paths.
        let cors = CorsLayer::new()
            .allow_methods([Method::GET])
            .allow_origin(Any);

        // Mount the .well-known endpoint at the path derived from the resource URL
        let resource_path = self.resource.path().trim_end_matches('/');
        let well_known_path = format!("{}/.well-known/oauth-protected-resource", resource_path);

        let auth_router = Router::new()
            .route(&well_known_path, get(protected_resource))
            .with_state(self.clone())
            .layer(cors);

        // Merge with MCP server routes
        Router::new()
            .merge(auth_router)
            .merge(router.layer(axum::middleware::from_fn_with_state(
                self.clone(),
                oauth_validate,
            )))
    }
}

/// Validate that requests made have a corresponding bearer JWT token
#[tracing::instrument(skip_all, fields(status_code, reason))]
async fn oauth_validate(
    State(auth_config): State<Config>,
    token: Option<TypedHeader<Authorization<Bearer>>>,
    mut request: Request,
    next: Next,
) -> Result<Response, (StatusCode, TypedHeader<WwwAuthenticate>)> {
    // Consolidated unauthorized error for use with any fallible step in this process
    let unauthorized_error = || {
        let mut resource = auth_config.resource.clone();
        // Preserve the configured path and append the well-known endpoint
        let current_path = resource.path().trim_end_matches('/');
        resource.set_path(&format!(
            "{}/.well-known/oauth-protected-resource",
            current_path
        ));

        (
            StatusCode::UNAUTHORIZED,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource,
            }),
        )
    };

    let validator = NetworkedTokenValidator::new(&auth_config.audiences, &auth_config.servers);
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
            resource: Url::parse("http://localhost:4000").unwrap(),
            resource_documentation: None,
            scopes: vec!["read".to_string()],
            disable_auth_token_passthrough: false,
        }
    }

    fn test_config_with_path(path: &str) -> Config {
        Config {
            servers: vec![Url::parse("http://localhost:1234").unwrap()],
            audiences: vec!["test-audience".to_string()],
            resource: Url::parse(&format!("http://localhost:4000{}", path)).unwrap(),
            resource_documentation: None,
            scopes: vec!["read".to_string()],
            disable_auth_token_passthrough: false,
        }
    }

    fn test_router(config: Config) -> Router {
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(from_fn_with_state(config, oauth_validate))
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
    async fn resource_metadata_url_preserves_path() {
        let config = test_config_with_path("/custom-path");
        let app = test_router(config.clone());
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let www_auth = res
            .headers()
            .get(WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            www_auth.contains("/custom-path/.well-known/oauth-protected-resource"),
            "Expected resource_metadata to include custom path, got: {}",
            www_auth
        );
    }

    #[tokio::test]
    async fn resource_metadata_url_works_without_path() {
        let config = test_config();
        let app = test_router(config.clone());
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let www_auth = res
            .headers()
            .get(WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            www_auth.contains("http://localhost:4000/.well-known/oauth-protected-resource"),
            "Expected resource_metadata at root, got: {}",
            www_auth
        );
    }

    #[tokio::test]
    async fn resource_metadata_url_handles_trailing_slash() {
        let config = test_config_with_path("/custom-path/");
        let app = test_router(config.clone());
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let www_auth = res
            .headers()
            .get(WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            www_auth.contains("/custom-path/.well-known/oauth-protected-resource"),
            "Expected clean path without double slashes, got: {}",
            www_auth
        );
        assert!(
            !www_auth.contains("4000//"),
            "Should not contain double slashes after host, got: {}",
            www_auth
        );
    }

    #[tokio::test]
    async fn well_known_endpoint_accessible_at_custom_path() {
        let config = test_config_with_path("/custom-path");
        let app = config.enable_middleware(Router::new());

        let req = Request::builder()
            .uri("/custom-path/.well-known/oauth-protected-resource")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn well_known_endpoint_accessible_at_root() {
        let config = test_config();
        let app = config.enable_middleware(Router::new());

        let req = Request::builder()
            .uri("/.well-known/oauth-protected-resource")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn well_known_endpoint_not_at_root_when_custom_path() {
        let config = test_config_with_path("/custom-path");
        let app = config.enable_middleware(Router::new());

        let req = Request::builder()
            .uri("/.well-known/oauth-protected-resource")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_ne!(res.status(), StatusCode::OK);
    }
}
