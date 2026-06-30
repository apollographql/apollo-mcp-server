use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::KeyAlgorithm;
use jwks::{Jwk, Jwks};
use serde::Deserialize;
use tracing::{error, info, trace, warn};
use url::Url;

use super::valid_token::KeyResolver;

pub(super) struct CachedJwks {
    pub keys: Jwks,
    pub issuer: String,
    pub fetched_at: Instant,
    /// Algorithms advertised by the discovery document, used as a fallback
    /// when a JWK omits `alg`.
    pub signing_algs: Vec<String>,
}

impl CachedJwks {
    pub fn is_fresh(&self, ttl: Duration) -> bool {
        self.fetched_at.elapsed() < ttl
    }

    /// Returns the JWK for `key_id`, filling in `alg` from the discovery
    /// document if the JWK itself omits it.
    fn lookup(&self, key_id: &str, server: &Url) -> Option<(Jwk, String)> {
        let mut jwk = self.keys.keys.get(key_id)?.clone();
        if jwk.alg.is_none() {
            jwk.alg = resolve_alg(&self.signing_algs, server);
        }
        Some((jwk, self.issuer.clone()))
    }
}

/// [`KeyResolver`] that fetches signing keys from the network via OIDC/OAuth
/// discovery.
pub(super) struct NetworkedKeyResolver<'a> {
    client: &'a reqwest::Client,
    discovery_timeout: Duration,
    jwks_cache: &'a Arc<RwLock<HashMap<Url, CachedJwks>>>,
    ttl: Duration,
}

impl<'a> NetworkedKeyResolver<'a> {
    pub fn new(
        client: &'a reqwest::Client,
        discovery_timeout: Duration,
        jwks_cache: &'a Arc<RwLock<HashMap<Url, CachedJwks>>>,
        ttl: Duration,
    ) -> Self {
        Self {
            client,
            discovery_timeout,
            jwks_cache,
            ttl,
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

    let path = super::normalized_path_segments(&normalized);

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

/// Subset of the OIDC/OAuth discovery document that we consume.
///
/// `jwks_uri` locates the public keys. `id_token_signing_alg_values_supported`
/// is strictly the algorithm list for OIDC ID tokens, but MCP validates OAuth
/// access tokens — no standard discovery field advertises the access-token
/// signing algorithm. In practice, the providers this fallback targets
/// (Microsoft Entra ID, Azure AD B2C, AWS Cognito, Ping Identity) sign access
/// tokens with the same algorithm they advertise here, so we reuse this field
/// as a proxy to fill in `alg` when a JWK omits it (RFC 7517 §4.4).
#[derive(Debug, Deserialize)]
struct DiscoveryMetadata {
    /// The authorization server's issuer identifier. RFC 8414 and OpenID
    /// Connect Discovery both require this field, and it must equal the `iss`
    /// claim of tokens the server issues (OpenID Connect Core §2, RFC 9068
    /// §2.2). Used to bind issuer validation to the server whose JWKS verified
    /// the signature.
    issuer: String,
    jwks_uri: String,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
}

/// Fetches the discovery document, trying each well-known URL in priority order.
async fn discover_metadata(
    client: &reqwest::Client,
    issuer: &Url,
    timeout: Duration,
) -> Option<DiscoveryMetadata> {
    let Ok(urls) = build_discovery_urls(issuer)
        .inspect_err(|e| warn!(error = %e, "Failed to build discovery URLs"))
    else {
        return None;
    };

    for url in &urls {
        let fetch = async {
            client
                .get(url.as_str())
                .send()
                .await?
                .error_for_status()?
                .json::<DiscoveryMetadata>()
                .await
        };

        match tokio::time::timeout(timeout, fetch).await {
            Ok(Ok(metadata)) => {
                info!(url = %url, "Authorization server metadata discovered");
                return Some(metadata);
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

/// Fetches the JWKS from `jwks_uri`.
async fn fetch_jwks(client: &reqwest::Client, jwks_uri: &str, timeout: Duration) -> Option<Jwks> {
    match tokio::time::timeout(timeout, Jwks::from_jwks_url_with_client(client, jwks_uri)).await {
        Ok(Ok(jwks)) => Some(jwks),
        Ok(Err(e)) => {
            warn!(jwks_uri = %jwks_uri, error = %e, "Failed to fetch JWKS");
            None
        }
        Err(_) => {
            warn!(jwks_uri = %jwks_uri, timeout_secs = ?timeout.as_secs(), "JWKS fetch timed out");
            None
        }
    }
}

impl KeyResolver for NetworkedKeyResolver<'_> {
    /// `discovery_timeout` bounds each network stage (metadata fetch and JWKS
    /// fetch) independently, so a cold-cache lookup can take up to 2×
    /// `discovery_timeout` on the happy path. The JWKS fetch does not fall
    /// back to alternate discovery URLs on failure; real providers advertise
    /// the same `jwks_uri` from every well-known path.
    async fn resolve_key(&self, server: &Url, key_id: &str) -> Option<(Jwk, String)> {
        // Return immediately if the key is cached and fresh.
        if let Ok(cache) = self.jwks_cache.read()
            && let Some(entry) = cache.get(server)
            && entry.is_fresh(self.ttl)
            && let Some(result) = entry.lookup(key_id, server)
        {
            return Some(result);
        }

        let metadata = discover_metadata(self.client, server, self.discovery_timeout).await?;
        let jwks = fetch_jwks(self.client, &metadata.jwks_uri, self.discovery_timeout).await?;

        // Re-check before inserting; another request may have populated the
        // cache during the network fetch. Recover from lock poison: inserting
        // a fresh value is safe regardless of prior state.
        let mut cache = self.jwks_cache.write().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.get(server)
            && entry.is_fresh(self.ttl)
            && let Some(result) = entry.lookup(key_id, server)
        {
            return Some(result);
        }
        let entry = CachedJwks {
            keys: jwks,
            issuer: metadata.issuer,
            fetched_at: Instant::now(),
            signing_algs: metadata.id_token_signing_alg_values_supported,
        };
        let result = entry.lookup(key_id, server);
        cache.insert(server.clone(), entry);
        result
    }
}

/// Resolves the signing algorithm from the list of algorithms the authorization
/// server advertises in its discovery document, for use when a JWK omits `alg`.
///
/// Returns `None` unless the server advertises exactly one algorithm and we
/// recognize it. Any additional or unrecognized entry counts as ambiguity and
/// is refused to avoid an algorithm-confusion attack.
fn resolve_alg(advertised: &[String], server: &Url) -> Option<KeyAlgorithm> {
    let [single] = advertised else {
        if advertised.is_empty() {
            warn!(
                server = %server,
                "Authorization server discovery did not advertise any signing algorithms and the JWK omits `alg`; tokens signed by this key cannot be verified"
            );
        } else {
            error!(
                server = %server,
                advertised = ?advertised,
                "Authorization server advertises multiple signing algorithms but the JWK omits `alg`; Apollo MCP Server cannot safely pick one"
            );
        }
        return None;
    };

    KeyAlgorithm::from_str(single)
        .inspect_err(|_| {
            error!(
                server = %server,
                alg = %single,
                "Authorization server discovery advertises an unrecognized signing algorithm and the JWK omits `alg`; cannot safely infer the algorithm"
            );
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::KeyResolver;
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
    fn discovery_urls_match_expected(#[case] issuer: &str, #[case] expected: Vec<&str>) {
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
    fn returns_error_for_missing_host() {
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
    async fn discover_metadata_returns_metadata_when_first_url_succeeds() {
        let mut server = mockito::Server::new_async().await;
        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
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

        let client = reqwest::Client::new();
        let issuer = Url::parse(&server.url()).expect("mock server URL should be valid");

        let result = discover_metadata(&client, &issuer, Duration::from_secs(5)).await;

        discovery_mock.assert();
        let metadata = result.expect("discovery should succeed");
        assert_eq!(metadata.jwks_uri, format!("{}/jwks", server.url()));
        assert_eq!(
            metadata.id_token_signing_alg_values_supported,
            vec!["RS256".to_string()]
        );
    }

    #[tokio::test]
    async fn discover_metadata_falls_back_when_rfc8414_returns_404() {
        let mut server = mockito::Server::new_async().await;
        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks"}}"#,
            server.url(),
            server.url()
        );

        let fail_mock = server
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;

        let oidc_mock = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&discovery_json)
            .expect(1)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let issuer = Url::parse(&server.url()).expect("mock server URL should be valid");

        let result = discover_metadata(&client, &issuer, Duration::from_secs(5)).await;

        fail_mock.assert();
        oidc_mock.assert();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn discover_metadata_returns_none_when_all_urls_fail() {
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

        let result = discover_metadata(&client, &issuer, Duration::from_secs(5)).await;

        fail_mock1.assert();
        fail_mock2.assert();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn discover_metadata_defaults_algorithms_to_empty_when_field_missing() {
        let mut server = mockito::Server::new_async().await;
        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks"}}"#,
            server.url(),
            server.url()
        );

        let mock = server
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&discovery_json)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let issuer = Url::parse(&server.url()).expect("mock server URL should be valid");

        let result = discover_metadata(&client, &issuer, Duration::from_secs(5)).await;

        mock.assert();
        let metadata = result.expect("discovery should succeed");
        assert!(metadata.id_token_signing_alg_values_supported.is_empty());
    }

    #[tokio::test]
    async fn fetch_jwks_returns_jwks_on_success() {
        let mut server = mockito::Server::new_async().await;
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"test-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );

        let mock = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&jwks_json)
            .expect(1)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let jwks_uri = format!("{}/jwks", server.url());

        let result = fetch_jwks(&client, &jwks_uri, Duration::from_secs(5)).await;

        mock.assert();
        let jwks = result.expect("fetch should succeed");
        assert!(jwks.keys.contains_key("test-key"));
    }

    #[test]
    fn resolve_alg_picks_single_advertised_alg() {
        let server = Url::parse("https://auth.example.com").expect("test URL should be valid");
        let result = resolve_alg(&["RS256".to_string()], &server);
        assert_eq!(result, Some(KeyAlgorithm::RS256));
    }

    #[test]
    fn resolve_alg_rejects_when_multiple_advertised() {
        let server = Url::parse("https://auth.example.com").expect("test URL should be valid");
        let result = resolve_alg(&["RS256".to_string(), "PS256".to_string()], &server);
        assert!(result.is_none());
    }

    #[test]
    fn resolve_alg_returns_none_when_empty() {
        let server = Url::parse("https://auth.example.com").expect("test URL should be valid");
        let result = resolve_alg(&[], &server);
        assert!(result.is_none());
    }

    #[test]
    fn resolve_alg_rejects_unrecognized_sibling_entry() {
        let server = Url::parse("https://auth.example.com").expect("test URL should be valid");
        let result = resolve_alg(&["RS256".to_string(), "none".to_string()], &server);
        assert!(result.is_none());
    }

    #[test]
    fn resolve_alg_rejects_single_unrecognized_entry() {
        let server = Url::parse("https://auth.example.com").expect("test URL should be valid");
        let result = resolve_alg(&["BOGUS".to_string()], &server);
        assert!(result.is_none());
    }

    /// Build a real `Jwks` for pre-populating the cache. `Jwks` doesn't impl
    /// `Deserialize`, so we spin up a throwaway mockito server and reuse
    /// `fetch_jwks` to construct one.
    async fn make_test_jwks(client: &reqwest::Client, jwks_json: &str) -> Jwks {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(jwks_json)
            .create_async()
            .await;
        fetch_jwks(
            client,
            &format!("{}/jwks", server.url()),
            Duration::from_secs(5),
        )
        .await
        .expect("test setup: jwks fetch failed")
    }

    #[tokio::test]
    async fn warm_hit_returns_without_network() {
        let client = reqwest::Client::new();

        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"cached-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );
        let jwks = make_test_jwks(&client, &jwks_json).await;

        // Actual test server — any request reaching here means the warm path failed.
        let mut server = mockito::Server::new_async().await;
        let no_network = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let issuer_url = Url::parse(&server.url()).expect("valid URL");

        let cache: Arc<RwLock<HashMap<Url, CachedJwks>>> = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut guard = cache.write().unwrap();
            guard.insert(
                issuer_url.clone(),
                CachedJwks {
                    keys: jwks,
                    issuer: "https://expected-issuer.example.com".to_string(),
                    fetched_at: Instant::now(),
                    signing_algs: vec!["RS256".to_string()],
                },
            );
        }

        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &cache,
            Duration::from_secs(300),
        );

        let result = resolver.resolve_key(&issuer_url, "cached-key").await;

        no_network.assert();
        let (_jwk, issuer) = result.expect("warm hit should return Some");
        assert_eq!(issuer, "https://expected-issuer.example.com");
    }

    #[tokio::test]
    async fn warm_hit_fills_alg_from_signing_algs_when_jwk_omits_it() {
        let client = reqwest::Client::new();

        // JWK without `alg`; algorithm comes from the discovery document.
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"no-alg-key","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );
        let jwks = make_test_jwks(&client, &jwks_json).await;

        let mut server = mockito::Server::new_async().await;
        let no_network = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let issuer_url = Url::parse(&server.url()).expect("valid URL");

        let cache: Arc<RwLock<HashMap<Url, CachedJwks>>> = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut guard = cache.write().unwrap();
            guard.insert(
                issuer_url.clone(),
                CachedJwks {
                    keys: jwks,
                    issuer: "https://issuer.example.com".to_string(),
                    fetched_at: Instant::now(),
                    signing_algs: vec!["RS256".to_string()],
                },
            );
        }

        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &cache,
            Duration::from_secs(300),
        );

        let result = resolver.resolve_key(&issuer_url, "no-alg-key").await;

        no_network.assert();
        let (jwk, _issuer) = result.expect("warm hit should return Some");
        assert_eq!(jwk.alg, Some(KeyAlgorithm::RS256));
    }

    #[tokio::test]
    async fn cold_miss_populates_cache() {
        let mut server = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server.url(),
            server.url()
        );
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"fresh-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
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

        let issuer_url = Url::parse(&server.url()).expect("valid URL");
        let cache: Arc<RwLock<HashMap<Url, CachedJwks>>> = Arc::new(RwLock::new(HashMap::new()));
        let client = reqwest::Client::new();
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &cache,
            Duration::from_secs(300),
        );

        let result = resolver.resolve_key(&issuer_url, "fresh-key").await;

        discovery_mock.assert();
        jwks_mock.assert();
        let (_jwk, _issuer) = result.expect("cold miss should return Some");

        let guard = cache.read().unwrap();
        let entry = guard.get(&issuer_url).expect("cache should have an entry");
        assert!(entry.keys.keys.contains_key("fresh-key"));
        assert!(entry.is_fresh(Duration::from_secs(300)));
    }

    #[tokio::test]
    async fn expired_entry_triggers_refetch() {
        let client = reqwest::Client::new();

        let stale_jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"old-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );
        let stale_jwks = make_test_jwks(&client, &stale_jwks_json).await;

        let mut server = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server.url(),
            server.url()
        );
        let fresh_jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"new-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
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
            .with_body(&fresh_jwks_json)
            .expect(1)
            .create_async()
            .await;

        let issuer_url = Url::parse(&server.url()).expect("valid URL");
        let cache: Arc<RwLock<HashMap<Url, CachedJwks>>> = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut guard = cache.write().unwrap();
            guard.insert(
                issuer_url.clone(),
                CachedJwks {
                    keys: stale_jwks,
                    issuer: server.url(),
                    fetched_at: Instant::now(),
                    signing_algs: vec!["RS256".to_string()],
                },
            );
        }

        // 1ms TTL — the entry will expire almost immediately
        let ttl = Duration::from_millis(1);
        let resolver = NetworkedKeyResolver::new(&client, Duration::from_secs(5), &cache, ttl);

        // Let the entry expire
        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = resolver.resolve_key(&issuer_url, "new-key").await;

        discovery_mock.assert();
        jwks_mock.assert();
        let (_jwk, _issuer) = result.expect("expired entry should trigger refetch");

        let guard = cache.read().unwrap();
        let entry = guard.get(&issuer_url).expect("cache should be repopulated");
        assert!(entry.keys.keys.contains_key("new-key"));
        assert!(!entry.keys.keys.contains_key("old-key"));
    }
}
