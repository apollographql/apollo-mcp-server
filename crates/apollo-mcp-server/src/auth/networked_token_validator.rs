use std::time::Duration;

use jwks::{Jwk, Jwks};
use tracing::{info, trace, warn};
use url::Url;

use super::valid_token::ValidateToken;

/// Implementation of the `ValidateToken` trait which fetches key information
/// from the network.
pub(super) struct NetworkedTokenValidator<'a> {
    audiences: &'a [String],
    allow_any_audience: bool,
    upstreams: &'a Vec<Url>,
    client: &'a reqwest::Client,
    discovery_timeout: Duration,
}

impl<'a> NetworkedTokenValidator<'a> {
    pub fn new(
        audiences: &'a [String],
        allow_any_audience: bool,
        upstreams: &'a Vec<Url>,
        client: &'a reqwest::Client,
        discovery_timeout: Duration,
    ) -> Self {
        Self {
            audiences,
            allow_any_audience,
            upstreams,
            client,
            discovery_timeout,
        }
    }
}

/// Error type for discovery URL construction failures.
#[derive(Debug, thiserror::Error)]
enum DiscoveryUrlError {
    #[error("issuer URL has no host: {0}")]
    MissingHost(Url),
}

/// Constructs discovery URLs. Returns URLs in priority order:
/// 1. RFC 8414 (path-insertion): `/.well-known/oauth-authorization-server/{path}`
/// 2. OIDC Discovery (path-insertion): `/.well-known/openid-configuration/{path}`
/// 3. OIDC Discovery (legacy path-appending): `/{path}/.well-known/openid-configuration`
///
/// # URL Normalization
/// Query strings and fragments are stripped per RFC 8414 Section 3.1.
/// The spec does not define behavior for these, and most implementations strip them.
///
/// # Errors
/// Returns `DiscoveryUrlError::MissingHost` if the issuer URL lacks a host.
fn build_discovery_urls(issuer: &Url) -> Result<Vec<Url>, DiscoveryUrlError> {
    let mut normalized = issuer.clone();
    normalized.set_query(None);
    normalized.set_fragment(None);

    let path = normalized
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");

    let Some(host) = normalized.host_str() else {
        return Err(DiscoveryUrlError::MissingHost(issuer.clone()));
    };

    let origin = format!("{}://{}", normalized.scheme(), host);

    // Add port if non-standard
    let origin = if let Some(port) = normalized.port() {
        format!("{}:{}", origin, port)
    } else {
        origin
    };

    let path_suffix = if path.is_empty() {
        String::new()
    } else {
        format!("/{}", path)
    };

    let mut urls = vec![
        // Priority 1: RFC 8414 path-insertion
        format!(
            "{}/.well-known/oauth-authorization-server{}",
            origin, path_suffix
        ),
        // Priority 2: OIDC Discovery path-insertion
        format!("{}/.well-known/openid-configuration{}", origin, path_suffix),
    ];

    // Priority 3: OIDC Discovery legacy path-appending (only if there IS a path)
    if !path.is_empty() {
        urls.push(format!(
            "{}/{}/.well-known/openid-configuration",
            origin, path
        ));
    }

    Ok(urls
        .into_iter()
        .filter_map(|s| {
            Url::parse(&s)
                .inspect_err(|e| trace!(url = %s, error = %e, "Failed to parse discovery URL"))
                .ok()
        })
        .collect())
}

/// Attempts discovery from multiple URLs sequentially, returning first success
async fn discover_jwks(client: &reqwest::Client, issuer: &Url, timeout: Duration) -> Option<Jwks> {
    let urls = match build_discovery_urls(issuer) {
        Ok(urls) => urls,
        Err(e) => {
            warn!(error = %e, "Failed to build discovery URLs");
            return None;
        }
    };

    for url in &urls {
        let result = tokio::time::timeout(
            timeout,
            Jwks::from_oidc_url_with_client(client, url.as_str()),
        )
        .await;

        match result {
            Ok(Ok(jwks)) => {
                info!(url = %url, "Authorization server metadata discovered");
                return Some(jwks);
            }
            Ok(Err(e)) => {
                trace!(url = %url, error = %e, "Discovery failed, trying next URL");
            }
            Err(_) => {
                trace!(url = %url, timeout_secs = ?timeout.as_secs(), "Discovery timed out, trying next URL");
            }
        }
    }

    warn!(issuer = %issuer, "All discovery URLs failed");
    None
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
        let jwks = discover_jwks(self.client, server, self.discovery_timeout).await?;
        jwks.keys.get(key_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // No path - returns 2 URLs (no legacy path-appending)
    #[case(
        "https://auth.example.com",
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server",
            "https://auth.example.com/.well-known/openid-configuration",
        ]
    )]
    // Single path segment - returns 3 URLs
    #[case(
        "https://auth.example.com/tenant1",
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
            "https://auth.example.com/.well-known/openid-configuration/tenant1",
            "https://auth.example.com/tenant1/.well-known/openid-configuration",
        ]
    )]
    // Deep path (Keycloak style) - returns 3 URLs with full path
    #[case(
        "https://sso.company.com/auth/realms/my-realm",
        vec![
            "https://sso.company.com/.well-known/oauth-authorization-server/auth/realms/my-realm",
            "https://sso.company.com/.well-known/openid-configuration/auth/realms/my-realm",
            "https://sso.company.com/auth/realms/my-realm/.well-known/openid-configuration",
        ]
    )]
    // Trailing slash is normalized
    #[case(
        "https://auth.example.com/tenant1/",
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
            "https://auth.example.com/.well-known/openid-configuration/tenant1",
            "https://auth.example.com/tenant1/.well-known/openid-configuration",
        ]
    )]
    // Query string is stripped per RFC 8414
    #[case(
        "https://auth.example.com/tenant1?version=2",
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
            "https://auth.example.com/.well-known/openid-configuration/tenant1",
            "https://auth.example.com/tenant1/.well-known/openid-configuration",
        ]
    )]
    // Fragment is stripped
    #[case(
        "https://auth.example.com/tenant1#section",
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
            "https://auth.example.com/.well-known/openid-configuration/tenant1",
            "https://auth.example.com/tenant1/.well-known/openid-configuration",
        ]
    )]
    // Non-standard port is preserved
    #[case(
        "https://localhost:8443/tenant1",
        vec![
            "https://localhost:8443/.well-known/oauth-authorization-server/tenant1",
            "https://localhost:8443/.well-known/openid-configuration/tenant1",
            "https://localhost:8443/tenant1/.well-known/openid-configuration",
        ]
    )]
    // Auth0 style (no path)
    #[case(
        "https://dev-abc123.us.auth0.com",
        vec![
            "https://dev-abc123.us.auth0.com/.well-known/oauth-authorization-server",
            "https://dev-abc123.us.auth0.com/.well-known/openid-configuration",
        ]
    )]
    // WorkOS style (no path, with trailing slash normalized)
    #[case(
        "https://abb-123-staging.authkit.app/",
        vec![
            "https://abb-123-staging.authkit.app/.well-known/oauth-authorization-server",
            "https://abb-123-staging.authkit.app/.well-known/openid-configuration",
        ]
    )]
    fn test_build_discovery_urls(#[case] issuer: &str, #[case] expected: Vec<&str>) {
        let issuer_url = Url::parse(issuer).expect("valid test URL");
        let urls: Vec<String> = build_discovery_urls(&issuer_url)
            .expect("should build discovery URLs")
            .iter()
            .map(|u| u.as_str().to_string())
            .collect();
        assert_eq!(urls, expected);
    }

    #[test]
    fn double_slashes_in_path_are_collapsed() {
        let issuer = Url::parse("https://auth.example.com//tenant1//")
            .expect("test issuer URL should be valid");
        let urls = build_discovery_urls(&issuer).expect("should build discovery URLs");

        // Double slashes should be normalized to single path segment
        assert_eq!(
            urls.first().map(|u| u.as_str()),
            Some("https://auth.example.com/.well-known/oauth-authorization-server/tenant1")
        );
    }

    #[test]
    fn build_discovery_urls_returns_error_for_missing_host() {
        // A file:// URL typically has no host
        let issuer =
            Url::parse("file:///path/to/something").expect("test file URL should be valid");
        let result = build_discovery_urls(&issuer);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DiscoveryUrlError::MissingHost(_)));
        assert!(err.to_string().contains("issuer URL has no host"));
    }

    // Example RSA public key components from RFC 7517 Appendix A.1
    // These are well-known test vectors - public key only, no private material
    // https://datatracker.ietf.org/doc/html/rfc7517#appendix-A.1
    const TEST_RSA_N: &str = "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw";
    const TEST_RSA_E: &str = "AQAB";

    #[tokio::test]
    async fn discover_jwks_should_return_jwks_when_first_url_succeeds() {
        // given
        let mut server = mockito::Server::new_async().await;
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"test-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );
        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks"}}"#,
            server.url(),
            server.url()
        );

        let discovery_mock = server
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&discovery_json)
            .expect(1)
            .create_async()
            .await;

        let jwks_mock = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&jwks_json)
            .expect(1)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let issuer = Url::parse(&server.url()).expect("mock server URL should be valid");

        // when
        let result = discover_jwks(&client, &issuer, Duration::from_secs(5)).await;

        // then
        discovery_mock.assert();
        jwks_mock.assert();
        let jwks = result.expect("discover_jwks should return Some when first URL succeeds");
        assert!(
            jwks.keys.contains_key("test-key"),
            "Expected test-key in discovered JWKS"
        );
    }

    #[tokio::test]
    async fn discover_jwks_should_fallback_to_oidc_when_rfc8414_returns_404() {
        // given
        let mut server = mockito::Server::new_async().await;
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"fallback-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );
        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks"}}"#,
            server.url(),
            server.url()
        );

        // First URL (RFC 8414) fails with 404
        let fail_mock = server
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;

        // Second URL (OIDC) succeeds
        let discovery_mock = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&discovery_json)
            .expect(1)
            .create_async()
            .await;

        let jwks_mock = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&jwks_json)
            .expect(1)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let issuer = Url::parse(&server.url()).expect("mock server URL should be valid");

        // when
        let result = discover_jwks(&client, &issuer, Duration::from_secs(5)).await;

        // then
        fail_mock.assert();
        discovery_mock.assert();
        jwks_mock.assert();
        let jwks = result.expect("discover_jwks should fallback to OIDC when RFC 8414 returns 404");
        assert!(
            jwks.keys.contains_key("fallback-key"),
            "Expected fallback-key in discovered JWKS"
        );
    }

    #[tokio::test]
    async fn discover_jwks_should_return_none_when_all_urls_fail() {
        // given
        let mut server = mockito::Server::new_async().await;

        let fail_mock1 = server
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;

        let fail_mock2 = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(500)
            .expect(1)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let issuer = Url::parse(&server.url()).expect("mock server URL should be valid");

        // when
        let result = discover_jwks(&client, &issuer, Duration::from_secs(5)).await;

        // then
        fail_mock1.assert();
        fail_mock2.assert();
        assert!(result.is_none());
    }
}
