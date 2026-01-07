use jwks::{Jwk, Jwks};
use tracing::warn;
use url::Url;

use super::valid_token::ValidateToken;

/// Implementation of the `ValidateToken` trait which fetches key information
/// from the network.
pub(super) struct NetworkedTokenValidator<'a> {
    audiences: &'a [String],
    allow_any_audience: bool,
    upstreams: &'a Vec<Url>,
    client: &'a reqwest::Client,
}

impl<'a> NetworkedTokenValidator<'a> {
    pub fn new(
        audiences: &'a [String],
        allow_any_audience: bool,
        upstreams: &'a Vec<Url>,
        client: &'a reqwest::Client,
    ) -> Self {
        Self {
            audiences,
            allow_any_audience,
            upstreams,
            client,
        }
    }
}

/// Constructs the OIDC discovery URL by appending the well-known path to the oauth server URL.
fn build_oidc_url(oauth_server: &Url) -> Url {
    let mut discovery_url = oauth_server.clone();
    // This ensures Keycloak URLs like /auth/realms/<realm>/ work correctly.
    let current_path = discovery_url.path().trim_end_matches('/');
    discovery_url.set_path(&format!(
        "{current_path}/.well-known/oauth-authorization-server"
    ));
    discovery_url
}

impl ValidateToken for NetworkedTokenValidator<'_> {
    fn allow_any_audience(&self) -> bool {
        self.allow_any_audience
    }

    fn get_audiences(&self) -> &[String] {
        self.audiences
    }

    fn get_servers(&self) -> &Vec<Url> {
        self.upstreams
    }

    async fn get_key(&self, server: &Url, key_id: &str) -> Option<Jwk> {
        let oidc_url = build_oidc_url(server);

        let jwks = Jwks::from_oidc_url_with_client(self.client, oidc_url.as_str())
            .await
            .inspect_err(|e| {
                warn!("could not fetch OIDC information from {server}: {e}");
            })
            .ok()?;

        jwks.keys.get(key_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // Keycloak
    #[case(
        "https://sso.company.com/auth/realms/my-realm",
        "https://sso.company.com/auth/realms/my-realm/.well-known/oauth-authorization-server"
    )]
    #[case(
        "https://sso.company.com/auth/realms/my-realm/",
        "https://sso.company.com/auth/realms/my-realm/.well-known/oauth-authorization-server"
    )]
    // Auth0
    #[case(
        "https://dev-abc123.us.auth0.com",
        "https://dev-abc123.us.auth0.com/.well-known/oauth-authorization-server"
    )]
    // WorkOS
    #[case(
        "https://abb-123-staging.authkit.app/",
        "https://abb-123-staging.authkit.app/.well-known/oauth-authorization-server"
    )]
    fn test_build_oidc_discovery_url(#[case] input: &str, #[case] expected: &str) {
        let oauth_url = Url::parse(input).unwrap();
        let oidc_url = build_oidc_url(&oauth_url);

        assert_eq!(oidc_url.as_str(), expected);
    }
}
