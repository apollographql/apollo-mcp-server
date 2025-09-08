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

        (
            StatusCode::UNAUTHORIZED,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource,
            }),
        )
    };

    let validator = NetworkedTokenValidator::new(&auth_config.audiences, &auth_config.servers);
    let token = token.ok_or_else(unauthorized_error)?;

    let valid_token = validator
        .validate(token.0)
        .await
        .ok_or_else(unauthorized_error)?;

    // Insert new context to ensure that handlers only use our enforced token verification
    // for propagation
    request.extensions_mut().insert(valid_token);

    let response = next.run(request).await;
    Ok(response)
}
