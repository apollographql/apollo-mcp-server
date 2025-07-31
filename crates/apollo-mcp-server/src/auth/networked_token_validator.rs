use jwks::{Jwk, Jwks};
use tracing::warn;
use url::Url;

use super::valid_token::ValidateToken;

/// Implementation of the `ValidateToken` trait which fetches key information
/// from the network.
pub(super) struct NetworkedTokenValidator<'a> {
    audiences: &'a Vec<String>,
    upstreams: &'a Vec<Url>,
}

impl<'a> NetworkedTokenValidator<'a> {
    pub fn new(audiences: &'a Vec<String>, upstreams: &'a Vec<Url>) -> Self {
        Self {
            audiences,
            upstreams,
        }
    }
}

impl ValidateToken for NetworkedTokenValidator<'_> {
    fn get_audiences(&self) -> &Vec<String> {
        self.audiences
    }

    fn get_servers(&self) -> &Vec<Url> {
        self.upstreams
    }

    async fn get_key(&self, server: &Url, key_id: &str) -> Option<Jwk> {
        let oidc_url = {
            let mut server_url = server.clone();
            server_url.set_path("/.well-known/oauth-authorization-server");

            server_url
        };

        let jwks = Jwks::from_oidc_url(oidc_url)
            .await
            .inspect_err(|e| {
                warn!("could not fetch OIDC information from {server}: {e}");
            })
            .ok()?;

        jwks.keys.get(key_id).cloned()
    }
}
