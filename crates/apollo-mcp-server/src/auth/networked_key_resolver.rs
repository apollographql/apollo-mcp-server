use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
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

///  A future that does one cold-cache JWKS fetch and self-evicts from the inflight map.
pub(super) type InflightFetch = Shared<BoxFuture<'static, ()>>;

/// Per-issuer fetch coordination state, guarded by a single mutex.
#[derive(Default)]
pub(super) struct IssuerFetchState {
    /// In-flight cold fetches keyed by issuer URL.
    slots: HashMap<Url, InflightFetch>,
    /// When each issuer's most recent fetch started. Bounded by the
    /// configured server list.
    last_attempt: HashMap<Url, Instant>,
}

pub(super) type InflightMap = Mutex<IssuerFetchState>;

/// [`KeyResolver`] that fetches signing keys from the network via OIDC/OAuth
/// discovery.
pub(super) struct NetworkedKeyResolver<'a> {
    client: &'a reqwest::Client,
    discovery_timeout: Duration,
    inflight: &'a Arc<InflightMap>,
    jwks_cache: &'a Arc<RwLock<HashMap<Url, CachedJwks>>>,
    ttl: Duration,
    min_refresh_interval: Duration,
}

impl<'a> NetworkedKeyResolver<'a> {
    pub fn new(
        client: &'a reqwest::Client,
        discovery_timeout: Duration,
        inflight: &'a Arc<InflightMap>,
        jwks_cache: &'a Arc<RwLock<HashMap<Url, CachedJwks>>>,
        ttl: Duration,
        min_refresh_interval: Duration,
    ) -> Self {
        Self {
            client,
            discovery_timeout,
            inflight,
            jwks_cache,
            ttl,
            min_refresh_interval,
        }
    }

    /// Returns the resolved `(jwk, issuer)` if the cache holds a fresh entry
    /// containing `key_id`.
    fn lookup_fresh(&self, server: &Url, key_id: &str) -> Option<(Jwk, String)> {
        let cache = self.jwks_cache.read().unwrap_or_else(|e| e.into_inner());
        let entry = cache.get(server)?;
        if !entry.is_fresh(self.ttl) {
            return None;
        }
        entry.lookup(key_id, server)
    }

    /// Returns a future that completes when this issuer's fetch is done, or
    /// `None` if starting a fetch is not allowed right now.
    ///
    /// Sole owner of the fetch decision: at most one fetch per issuer is in
    /// flight, and at most one starts per `min_refresh_interval`. Joining an
    /// in-flight fetch is always allowed. The window is consumed when a
    /// fetch starts, even if it later fails.
    fn acquire_inflight_slot(&self, server: Url) -> Option<InflightFetch> {
        let mut guard = self.inflight.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(existing) = guard.slots.get(&server) {
            return Some(existing.clone());
        }

        if let Some(last) = guard.last_attempt.get(&server)
            && last.elapsed() < self.min_refresh_interval
        {
            return None;
        }

        let fetch = Self::build_fetch_future(
            self.client.clone(),
            Arc::clone(self.jwks_cache),
            Arc::clone(self.inflight),
            server.clone(),
            self.discovery_timeout,
        );

        guard.last_attempt.insert(server.clone(), Instant::now());
        guard.slots.insert(server, fetch.clone());
        Some(fetch)
    }

    /// Builds the one-shot future that fetches the issuer's keys, populates
    /// the cache, and removes its own inflight slot before resolving.
    fn build_fetch_future(
        client: reqwest::Client,
        jwks_cache: Arc<RwLock<HashMap<Url, CachedJwks>>>,
        inflight: Arc<InflightMap>,
        server: Url,
        discovery_timeout: Duration,
    ) -> InflightFetch {
        async move {
            let Some(metadata) = discover_metadata(&client, &server, discovery_timeout).await
            else {
                inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .slots
                    .remove(&server);
                return;
            };
            let Some(jwks) = fetch_jwks(&client, &metadata.jwks_uri, discovery_timeout).await
            else {
                inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .slots
                    .remove(&server);
                return;
            };

            let entry = CachedJwks {
                keys: jwks,
                issuer: metadata.issuer,
                fetched_at: Instant::now(),
                signing_algs: metadata.id_token_signing_alg_values_supported,
            };

            // Populate the cache before evicting the slot, so late arrivals
            // see the fresh entry instead of starting a new fetch.
            {
                let mut cache = jwks_cache.write().unwrap_or_else(|e| e.into_inner());
                cache.insert(server.clone(), entry);
            }
            inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .slots
                .remove(&server);
        }
        .boxed()
        .shared()
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
    /// Resolves from the in-memory cache when fresh. Concurrent misses for
    /// the same issuer share a single upstream fetch, and at most one fetch
    /// per issuer starts per `min_refresh_interval`; a miss inside the
    /// window is answered from the cache alone, with no outbound request.
    async fn resolve_key(&self, server: &Url, key_id: &str) -> Option<(Jwk, String)> {
        if let Some(result) = self.lookup_fresh(server, key_id) {
            return Some(result);
        }

        let Some(fetch) = self.acquire_inflight_slot(server.clone()) else {
            // Rate limited: re-read the cache before rejecting, since a
            // refresh finishing after the first lookup may have added this
            // `kid`.
            return self.lookup_fresh(server, key_id);
        };
        fetch.await;

        // Each caller looks up its own `kid` against the refreshed cache.
        self.lookup_fresh(server, key_id)
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

        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));

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
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
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

        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));

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
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
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

        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));

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
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
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

        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));

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
                    // Back-date so a 1ms TTL is already expired — no sleep needed.
                    fetched_at: Instant::now() - Duration::from_millis(10),
                    signing_algs: vec!["RS256".to_string()],
                },
            );
        }

        let ttl = Duration::from_millis(1);
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &inflight,
            &cache,
            ttl,
            Duration::from_secs(60),
        );

        let result = resolver.resolve_key(&issuer_url, "new-key").await;

        discovery_mock.assert();
        jwks_mock.assert();
        let (_jwk, _issuer) = result.expect("expired entry should trigger refetch");

        let guard = cache.read().unwrap();
        let entry = guard.get(&issuer_url).expect("cache should be repopulated");
        assert!(entry.keys.keys.contains_key("new-key"));
        assert!(!entry.keys.keys.contains_key("old-key"));
    }

    #[tokio::test]
    async fn concurrent_cold_misses_trigger_one_upstream_fetch_pair() {
        // 100 concurrent resolve_key calls for the same cold issuer must produce
        // exactly one discovery fetch and one JWKS fetch. mockito's .expect(1)
        // assertion enforces "exactly once."
        let mut server = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server.url(),
            server.url()
        );
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"singleflight-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
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
        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));
        let client = reqwest::Client::new();

        // Spawn 100 tasks that all race to resolve the same key against the same
        // cold cache. Each task constructs its own resolver borrowing the shared
        // client/cache/inflight handles.
        let mut handles = Vec::with_capacity(100);
        for _ in 0..100 {
            let client = client.clone();
            let cache = Arc::clone(&cache);
            let inflight = Arc::clone(&inflight);
            let issuer_url = issuer_url.clone();
            handles.push(tokio::spawn(async move {
                let resolver = NetworkedKeyResolver::new(
                    &client,
                    Duration::from_secs(5),
                    &inflight,
                    &cache,
                    Duration::from_secs(300),
                    Duration::from_secs(60),
                );
                resolver.resolve_key(&issuer_url, "singleflight-key").await
            }));
        }

        let results = futures::future::join_all(handles).await;

        // Every caller should observe the same Some(result).
        for r in results {
            let resolved = r.expect("task did not panic");
            let (_jwk, _issuer) = resolved.expect("resolve_key returned None under singleflight");
        }

        // The hard assertion: exactly one upstream pair.
        discovery_mock.assert();
        jwks_mock.assert();

        // And the inflight map self-evicted after the leader completed.
        let inflight_guard = inflight.lock().expect("inflight not poisoned");
        assert!(
            inflight_guard.slots.is_empty(),
            "inflight slot should be removed after the leader's future resolved"
        );
    }

    #[tokio::test]
    async fn kid_miss_inside_refresh_window_returns_none_without_network() {
        // After a cold fetch consumes the refresh window, an unknown kid
        // inside the window must be rejected with no additional upstream
        // requests.
        let mut server = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server.url(),
            server.url()
        );
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"known-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
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
        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));
        let client = reqwest::Client::new();
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
        );

        // Cold fetch: allowed, consumes the window.
        let first = resolver.resolve_key(&issuer_url, "known-key").await;
        assert!(first.is_some(), "cold fetch for a known kid should resolve");

        // Unknown kid inside the window: rejected, no second fetch.
        let second = resolver.resolve_key(&issuer_url, "bogus-kid").await;
        assert!(
            second.is_none(),
            "kid miss inside the refresh window should be rejected"
        );

        // A known kid still resolves from the warm cache.
        let third = resolver.resolve_key(&issuer_url, "known-key").await;
        assert!(
            third.is_some(),
            "warm hits are unaffected by the rate limit"
        );

        discovery_mock.assert();
        jwks_mock.assert();
    }

    #[tokio::test]
    async fn thousand_unique_kid_misses_trigger_one_upstream_fetch_pair() {
        // 1000 requests with unique bogus kids inside one refresh window
        // must produce exactly one upstream discovery + JWKS fetch pair.
        let mut server = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server.url(),
            server.url()
        );
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"real-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
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
        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));
        let client = reqwest::Client::new();
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
        );

        for i in 0..1000 {
            let kid = format!("attacker-kid-{i}");
            let result = resolver.resolve_key(&issuer_url, &kid).await;
            assert!(result.is_none(), "bogus kid {kid} must not resolve");
        }

        // The hard assertion: exactly one upstream pair for 1000 misses.
        discovery_mock.assert();
        jwks_mock.assert();
    }

    #[tokio::test]
    async fn refresh_allowed_again_after_window_elapses() {
        // A fetch must be allowed again once the refresh window has elapsed.
        // Back-dates last_attempt instead of sleeping, so the test is
        // deterministic.
        let mut server = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server.url(),
            server.url()
        );
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"rotated-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
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
        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));

        // Simulate a refresh that happened 120s ago with a 60s window.
        {
            let mut guard = inflight.lock().expect("inflight not poisoned");
            guard.last_attempt.insert(
                issuer_url.clone(),
                Instant::now() - Duration::from_secs(120),
            );
        }

        let client = reqwest::Client::new();
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
        );

        let result = resolver.resolve_key(&issuer_url, "rotated-key").await;

        discovery_mock.assert();
        jwks_mock.assert();
        assert!(
            result.is_some(),
            "fetch should be allowed once the window has elapsed"
        );
    }

    #[tokio::test]
    async fn rate_limit_is_per_issuer_not_global() {
        // Exhausting issuer A's window must not block issuer B's fetch.
        let mut server_b = mockito::Server::new_async().await;

        let discovery_json = format!(
            r#"{{"issuer":"{}","jwks_uri":"{}/jwks","id_token_signing_alg_values_supported":["RS256"]}}"#,
            server_b.url(),
            server_b.url()
        );
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"issuer-b-key","alg":"RS256","n":"{}","e":"{}"}}]}}"#,
            TEST_RSA_N, TEST_RSA_E
        );

        let discovery_mock = server_b
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&discovery_json)
            .expect(1)
            .create_async()
            .await;

        let jwks_mock = server_b
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&jwks_json)
            .expect(1)
            .create_async()
            .await;

        let issuer_a = Url::parse("https://issuer-a.example.com").expect("valid URL");
        let issuer_b = Url::parse(&server_b.url()).expect("valid URL");
        let cache: Arc<RwLock<HashMap<Url, CachedJwks>>> = Arc::new(RwLock::new(HashMap::new()));
        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));

        // Issuer A's window was just consumed.
        {
            let mut guard = inflight.lock().expect("inflight not poisoned");
            guard.last_attempt.insert(issuer_a.clone(), Instant::now());
        }

        let client = reqwest::Client::new();
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
        );

        // Issuer A is inside its window: rejected without touching the
        // network.
        let a = resolver.resolve_key(&issuer_a, "any-kid").await;
        assert!(a.is_none(), "issuer A is inside its window");

        // Issuer B: its own window is untouched; fetch proceeds.
        let b = resolver.resolve_key(&issuer_b, "issuer-b-key").await;
        discovery_mock.assert();
        jwks_mock.assert();
        assert!(b.is_some(), "issuer B has an independent refresh window");
    }

    #[tokio::test]
    async fn failed_fetch_consumes_the_refresh_window() {
        // A failing issuer is retried at most once per window: the first
        // call attempts discovery and fails; the second call inside the
        // window makes zero requests.
        let mut server = mockito::Server::new_async().await;

        // Both discovery URLs fail — one hit each on the first attempt only.
        let rfc8414_mock = server
            .mock("GET", "/.well-known/oauth-authorization-server")
            .with_status(500)
            .expect(1)
            .create_async()
            .await;
        let oidc_mock = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(500)
            .expect(1)
            .create_async()
            .await;

        let issuer_url = Url::parse(&server.url()).expect("valid URL");
        let cache: Arc<RwLock<HashMap<Url, CachedJwks>>> = Arc::new(RwLock::new(HashMap::new()));
        let inflight: Arc<InflightMap> = Arc::new(Mutex::new(IssuerFetchState::default()));
        let client = reqwest::Client::new();
        let resolver = NetworkedKeyResolver::new(
            &client,
            Duration::from_secs(5),
            &inflight,
            &cache,
            Duration::from_secs(300),
            Duration::from_secs(60),
        );

        let first = resolver.resolve_key(&issuer_url, "any-kid").await;
        assert!(
            first.is_none(),
            "fetch against a failing issuer returns None"
        );

        let second = resolver.resolve_key(&issuer_url, "any-kid").await;
        assert!(
            second.is_none(),
            "second attempt inside the window is rejected without HTTP"
        );

        // Exactly one hit per discovery URL across both calls.
        rfc8414_mock.assert();
        oidc_mock.assert();
    }
}
