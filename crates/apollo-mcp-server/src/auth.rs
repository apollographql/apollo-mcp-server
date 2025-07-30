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
use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use jwks::Jwks;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use url::Url;

mod valid_token;
mod www_authenticate;

pub(crate) use valid_token::ValidToken;
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
}

impl Config {
    pub fn enable_middleware(&self, router: axum::Router) -> axum::Router {
        #[derive(Serialize)]
        struct ProtectedResource {
            resource: Url,
            authorization_servers: Vec<Url>,
            bearer_methods_supported: Vec<String>,
            scopes_supported: Vec<String>,
            resource_documentation: String,
        }

        impl From<Config> for ProtectedResource {
            fn from(value: Config) -> Self {
                Self {
                    resource: value.resource,
                    authorization_servers: value.servers,
                    bearer_methods_supported: vec!["header".to_string()],
                    scopes_supported: ["profile", "email", "phone", "mcp-flow"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    resource_documentation: "TODO".to_string(),
                }
            }
        }

        async fn protected_resource(State(auth_config): State<Config>) -> Json<ProtectedResource> {
            Json(auth_config.into())
        }

        // Set up auth routes
        let cors = CorsLayer::new()
            // allow `GET` and `POST` when accessing the resource
            .allow_methods([Method::GET])
            // allow requests from any origin
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
    let resource_url = {
        let mut resource = auth_config.resource.clone();
        resource.set_path("/.well-known/oauth-protected-resource");

        resource
    };

    match token {
        Some(bearer) if token_is_valid(&auth_config, bearer.token()).await => {
            // Insert new context to ensure that handlers only use our enforced token verification
            // for propagation
            request
                .extensions_mut()
                .insert(ValidToken(bearer.token().to_string()));

            let response = next.run(request).await;
            Ok(response)
        }

        _ => Err((
            StatusCode::UNAUTHORIZED,
            TypedHeader(WwwAuthenticate::Bearer {
                resource_metadata: resource_url,
            }),
        )),
    }
}

/// Ensure that the supplied token is valid for the given config
async fn token_is_valid(auth_config: &Config, jwt: &str) -> bool {
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Claims {
        pub aud: String,
        pub sub: String,
    }

    let Ok(header) = decode_header(jwt) else {
        return false;
    };
    let Some(ref key_id) = header.kid else {
        return false;
    };

    for server in &auth_config.servers {
        let Ok(jwks) =
            Jwks::from_oidc_url(format!("{server}/.well-known/oauth-authorization-server"))
                .await
                .inspect_err(|e| {
                    warn!("could not fetch OIDC information from {server}: {e}. Skipping...");
                })
        else {
            continue;
        };

        let Some(jwk) = jwks.keys.get(key_id) else {
            continue;
        };

        let validation = {
            let mut val = Validation::new(match jwk.alg {
                jsonwebtoken::jwk::KeyAlgorithm::HS256 => Algorithm::HS256,
                jsonwebtoken::jwk::KeyAlgorithm::HS384 => Algorithm::HS384,
                jsonwebtoken::jwk::KeyAlgorithm::HS512 => Algorithm::HS512,
                jsonwebtoken::jwk::KeyAlgorithm::ES256 => Algorithm::ES256,
                jsonwebtoken::jwk::KeyAlgorithm::ES384 => Algorithm::ES384,
                jsonwebtoken::jwk::KeyAlgorithm::RS256 => Algorithm::RS256,
                jsonwebtoken::jwk::KeyAlgorithm::RS384 => Algorithm::RS384,
                jsonwebtoken::jwk::KeyAlgorithm::RS512 => Algorithm::RS512,
                jsonwebtoken::jwk::KeyAlgorithm::PS256 => Algorithm::PS256,
                jsonwebtoken::jwk::KeyAlgorithm::PS384 => Algorithm::PS384,
                jsonwebtoken::jwk::KeyAlgorithm::PS512 => Algorithm::PS512,
                jsonwebtoken::jwk::KeyAlgorithm::EdDSA => Algorithm::EdDSA,
                jsonwebtoken::jwk::KeyAlgorithm::RSA1_5 => todo!(),
                jsonwebtoken::jwk::KeyAlgorithm::RSA_OAEP => todo!(),
                jsonwebtoken::jwk::KeyAlgorithm::RSA_OAEP_256 => todo!(),
            });
            val.set_audience(&auth_config.audiences);

            val
        };
        match decode::<Claims>(jwt, &jwk.decoding_key, &validation) {
            Ok(claims) => {
                info!("Token passed validation with claims: {claims:?}");
                return true;
            }
            Err(e) => warn!("Token failed validation with error: {e}"),
        };
    }

    info!("Token did not pass validation");
    false
}
