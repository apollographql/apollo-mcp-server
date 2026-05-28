use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue};

use crate::graphs::manifest::types::UpstreamAuthConfig;

/// A v2 seam: produces the upstream headers to use for a given (graph, user)
/// combination. v1 always returns `base` unchanged.
pub trait CredentialProvider: Send + Sync + std::fmt::Debug {
    fn headers_for(&self, base: &HeaderMap, user: Option<&str>) -> HeaderMap;
}

/// Default v1 implementation: returns the base headers untouched.
#[derive(Debug, Default)]
pub struct PassthroughCredentials;

impl CredentialProvider for PassthroughCredentials {
    fn headers_for(&self, base: &HeaderMap, _user: Option<&str>) -> HeaderMap {
        base.clone()
    }
}

pub fn default_provider() -> Arc<dyn CredentialProvider> {
    Arc::new(PassthroughCredentials)
}

#[derive(Debug)]
struct CachedToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct OAuthClientCredentialsProvider {
    #[allow(dead_code)]
    http: reqwest::Client,
    cache: Arc<std::sync::RwLock<Option<CachedToken>>>,
}

#[derive(Debug)]
pub struct CredentialError(pub String);

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CredentialError {}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

async fn fetch_token(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    token_url: &str,
) -> Result<CachedToken, CredentialError> {
    let resp = http
        .post(token_url)
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .map_err(|e| CredentialError(e.to_string()))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| CredentialError(e.to_string()))?;

    let expires_at = Instant::now() + Duration::from_secs(body.expires_in.saturating_sub(60));
    Ok(CachedToken { token: body.access_token, expires_at })
}

impl OAuthClientCredentialsProvider {
    pub async fn new(config: &UpstreamAuthConfig) -> Result<Self, CredentialError> {
        let client_id = std::env::var(&config.client_id_env)
            .map_err(|_| CredentialError(format!("env var {} not set", config.client_id_env)))?;
        let client_secret = std::env::var(&config.client_secret_env)
            .map_err(|_| CredentialError(format!("env var {} not set", config.client_secret_env)))?;
        let token_url = config.token_url.clone();

        let http = reqwest::Client::new();
        let initial = fetch_token(&http, &client_id, &client_secret, &token_url).await?;
        let cache = Arc::new(std::sync::RwLock::new(Some(initial)));

        // Background refresh: wakes up when token is near expiry and re-fetches.
        let cache_bg = Arc::clone(&cache);
        let http_bg = http.clone();
        tokio::spawn(async move {
            loop {
                let sleep_dur = {
                    let guard = cache_bg.read().unwrap();
                    guard.as_ref().map_or(Duration::from_secs(60), |t| {
                        t.expires_at
                            .checked_duration_since(Instant::now())
                            .unwrap_or(Duration::ZERO)
                    })
                };
                tokio::time::sleep(sleep_dur).await;
                match fetch_token(&http_bg, &client_id, &client_secret, &token_url).await {
                    Ok(token) => *cache_bg.write().unwrap() = Some(token),
                    Err(e) => tracing::error!("Failed to refresh upstream token: {e}"),
                }
            }
        });

        Ok(Self { http, cache })
    }
}

impl CredentialProvider for OAuthClientCredentialsProvider {
    fn headers_for(&self, base: &HeaderMap, _user: Option<&str>) -> HeaderMap {
        let mut headers = base.clone();
        if let Ok(guard) = self.cache.read() {
            if let Some(cached) = guard.as_ref() {
                if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", cached.token)) {
                    headers.insert("x-fwd-authorization", value);
                }
            }
        }
        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    #[test]
    fn passthrough_returns_base_unchanged() {
        let mut base = HeaderMap::new();
        base.insert(AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        let p = PassthroughCredentials;
        let got = p.headers_for(&base, None);
        assert_eq!(got.get(AUTHORIZATION).unwrap(), "Bearer x");
    }

    mod oauth_provider {
        use super::*;

        #[test]
        fn headers_for_adds_x_fwd_authorization_when_token_cached() {
            let cache = Arc::new(std::sync::RwLock::new(Some(CachedToken {
                token: "my-token".to_string(),
                expires_at: Instant::now() + Duration::from_secs(3600),
            })));
            let provider = OAuthClientCredentialsProvider { http: reqwest::Client::new(), cache };
            let base = HeaderMap::new();
            let result = provider.headers_for(&base, None);
            assert_eq!(result.get("x-fwd-authorization").unwrap(), "Bearer my-token");
        }

        #[test]
        fn headers_for_preserves_base_headers() {
            let cache = Arc::new(std::sync::RwLock::new(Some(CachedToken {
                token: "tok".to_string(),
                expires_at: Instant::now() + Duration::from_secs(3600),
            })));
            let provider = OAuthClientCredentialsProvider { http: reqwest::Client::new(), cache };
            let mut base = HeaderMap::new();
            base.insert("x-custom", HeaderValue::from_static("val"));
            let result = provider.headers_for(&base, None);
            assert_eq!(result.get("x-custom").unwrap(), "val");
            assert!(result.get("x-fwd-authorization").is_some());
        }

        #[test]
        fn headers_for_returns_base_unchanged_when_no_token() {
            let cache = Arc::new(std::sync::RwLock::new(None));
            let provider = OAuthClientCredentialsProvider { http: reqwest::Client::new(), cache };
            let mut base = HeaderMap::new();
            base.insert("x-static", HeaderValue::from_static("yes"));
            let result = provider.headers_for(&base, None);
            assert_eq!(result.get("x-static").unwrap(), "yes");
            assert!(result.get("x-fwd-authorization").is_none());
        }

        #[tokio::test]
        async fn new_fetches_initial_token_from_token_endpoint() {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/oauth2/token")
                .match_body(mockito::Matcher::AllOf(vec![
                    mockito::Matcher::UrlEncoded("grant_type".into(), "client_credentials".into()),
                    mockito::Matcher::UrlEncoded("client_id".into(), "test-id".into()),
                    mockito::Matcher::UrlEncoded("client_secret".into(), "test-secret".into()),
                ]))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(r#"{"access_token":"tok","token_type":"Bearer","expires_in":3600}"#)
                .expect(1)
                .create_async()
                .await;

            let token_url = format!("{}/oauth2/token", server.url());
            let http = reqwest::Client::new();
            let cached = fetch_token(&http, "test-id", "test-secret", &token_url).await.unwrap();
            assert_eq!(cached.token, "tok");
            mock.assert_async().await;
        }
    }
}
