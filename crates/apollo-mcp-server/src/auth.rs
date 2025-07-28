use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
    routing::get,
};
use http::Method;
use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use jwks::{Jwks, JwksError};
use reqwest::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};
use url::Url;

/// Auth configuration options
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Config {
    /// Disable authentication
    #[serde(default)]
    pub disable: bool,

    /// List of upstream OAuth servers to delegate auth
    pub servers: Vec<Url>,

    /// List of accepted audiences for the OAuth tokens
    pub audiences: Vec<String>,

    /// The resource to protect.
    ///
    /// Note: This is usually the publically accessible URL of this running MCP server
    pub resource: String,
}

/// A validated token string
#[derive(Clone)]
pub struct ValidToken(String);

impl ValidToken {
    /// Read the contents of the token, consuming it.
    pub fn read(self) -> String {
        self.0
    }
}

// Note: Here we implement default for Config such that it disables auth when not
// specified. If it is specified, however, the above `#[serde(default)]` ensures that
// disabled is set to false.
impl Default for Config {
    fn default() -> Self {
        Self {
            disable: true,
            servers: Vec::new(),
            audiences: Vec::new(),
            resource: String::new(),
        }
    }
}

impl Config {
    pub fn enable_middleware(&self, router: axum::Router) -> axum::Router {
        #[derive(Serialize)]
        struct ProtectedResource {
            resource: String,
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

/// Helper for returning internal server errors
macro_rules! internal_error {
    ($msg:literal, $e:ident) => {{
        error!("INTERNAL ERROR (This should not happen). {}: {}", $msg, $e);

        (StatusCode::INTERNAL_SERVER_ERROR, Default::default())
    }};
}

async fn oauth_validate(
    State(auth_config): State<Config>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, (StatusCode, HeaderMap)> {
    match get_token(&headers) {
        Some(token)
            if token_is_valid(&auth_config, token)
                .await
                .map_err(|e| internal_error!("could not validate token", e))? =>
        {
            // Insert new context to ensure that handlers only use our enforced token verification
            // for propagation
            request
                .extensions_mut()
                .insert(ValidToken(token.to_string()));

            let response = next.run(request).await;
            Ok(response)
        }

        _ => Err((
            StatusCode::UNAUTHORIZED,
            HeaderMap::from_iter([(
                WWW_AUTHENTICATE,
                HeaderValue::from_str(&format!(
                    r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource""#,
                    auth_config.resource
                ))
                .map_err(|e| internal_error!("could not create resource metadata header", e))?,
            )]),
        )),
    }
}

fn get_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .map(HeaderValue::to_str)
        .transpose()
        .ok()
        .flatten()
}

async fn token_is_valid(auth_config: &Config, token: &str) -> Result<bool, JwksError> {
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Claims {
        pub aud: String,
        pub sub: String,
    }

    // The token should be in the form 'Bearer ...'
    let bearer_prefix = "bearer ";
    let Some((bearer, jwt)) = token.split_at_checked(bearer_prefix.len()) else {
        return Ok(false);
    };
    if bearer.to_lowercase() != bearer_prefix {
        return Ok(false);
    }

    let Ok(header) = decode_header(jwt) else {
        return Ok(false);
    };
    let Some(ref key_id) = header.kid else {
        return Ok(false);
    };

    for server in &auth_config.servers {
        let jwks =
            Jwks::from_oidc_url(format!("{server}/.well-known/oauth-authorization-server")).await?;

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
                return Ok(true);
            }
            Err(e) => warn!("Token failed validation with error: {e}"),
        };
    }

    info!("Token did not pass validation");
    Ok(false)
}
