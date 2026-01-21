use std::time::Duration;

use jwks::{Jwk, Jwks};
use tracing::{debug, info, warn};
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

/// Constructs discovery URLs per MCP Auth Spec 2025-11-25.
///
/// Returns URLs in priority order:
/// 1. RFC 8414 (path-insertion): `/.well-known/oauth-authorization-server/{path}`
/// 2. OIDC Discovery (path-insertion): `/.well-known/openid-configuration/{path}`
/// 3. OIDC Discovery (legacy path-appending): `/{path}/.well-known/openid-configuration`
///
/// # URL Normalization
/// Query strings and fragments are stripped per RFC 8414 Section 3.1.
/// The spec does not define behavior for these, and most implementations strip them.
fn build_discovery_urls(issuer: &Url) -> Vec<Url> {
    let mut normalized = issuer.clone();
    normalized.set_query(None);
    normalized.set_fragment(None);

    // Normalize path: remove leading/trailing slashes, collapse empty segments
    let path = normalized
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");

    let Some(host) = normalized.host_str() else {
        debug!(issuer = %issuer, "Issuer URL has no host, cannot build discovery URLs");
        return vec![];
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

    urls.into_iter()
        .filter_map(|s| {
            Url::parse(&s)
                .inspect_err(|e| debug!(url = %s, error = %e, "Failed to parse discovery URL"))
                .ok()
        })
        .collect()
}

/// Attempts discovery from multiple URLs sequentially, returning first success.
///
/// Uses per-URL timeouts to bound worst-case latency. Sequential is preferred over
/// parallel because:
/// - Happy path: 1 network request (most providers support at least one URL pattern)
/// - 404s are fast (~50ms), so fallback is quick for unsupported patterns
/// - Simpler to debug and reason about
/// - Less server load (parallel would fire 3 requests even when URL #1 works)
///
/// Worst case: 3 × timeout = 15s (bounded). If this becomes a problem in production,
/// we can revisit parallel approach based on telemetry.
async fn discover_jwks(client: &reqwest::Client, issuer: &Url) -> Option<Jwks> {
    let urls = build_discovery_urls(issuer);

    for url in &urls {
        // Per-URL timeout prevents slow failures from blocking the entire flow
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            Jwks::from_oidc_url_with_client(client, url.as_str()),
        )
        .await;

        match result {
            Ok(Ok(jwks)) => {
                info!(url = %url, "Authorization server metadata discovered");
                return Some(jwks);
            }
            Ok(Err(e)) => {
                // Fast failure (404, connection refused, parse error) - try next
                debug!(url = %url, error = %e, "Discovery failed, trying next URL");
            }
            Err(_) => {
                // Slow failure (timeout) - try next
                debug!(url = %url, "Discovery timed out after 5s, trying next URL");
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
        let jwks = discover_jwks(self.client, server).await?;
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
            .iter()
            .map(|u| u.as_str().to_string())
            .collect();
        assert_eq!(urls, expected);
    }

    #[test]
    fn double_slashes_in_path_are_collapsed() {
        let issuer = Url::parse("https://auth.example.com//tenant1//").expect("valid URL");
        let urls = build_discovery_urls(&issuer);

        // Double slashes should be normalized to single path segment
        assert_eq!(
            urls.first().map(|u| u.as_str()),
            Some("https://auth.example.com/.well-known/oauth-authorization-server/tenant1")
        );
    }
}
