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
use www_authenticate::WwwAuthenticate;

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
        if self.allow_any_audience {
            warn!(
                "allow_any_audience is enabled - audience validation is disabled. This reduces security."
            );
        }

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
        let auth_router = Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(protected_resource),
            )
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
        resource.set_path("/.well-known/oauth-protected-resource");

        let scope = if auth_config.scopes.is_empty() {
            None
        } else {
            Some(auth_config.scopes.join(" "))
        };

        (
            StatusCode::UNAUTHORIZED,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource,
                scope,
            }),
        )
    };

    let validator = NetworkedTokenValidator::new(
        &auth_config.audiences,
        auth_config.allow_any_audience,
        &auth_config.servers,
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
            resource: Url::parse("http://localhost:4000").unwrap(),
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
}
