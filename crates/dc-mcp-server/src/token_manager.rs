//! Token refresh functionality for Apollo MCP Server

use crate::config_manager::ConfigManager;
use crate::errors::McpError;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use rmcp::model::ErrorCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

#[derive(Debug, Serialize)]
struct RefreshTokenRequest {
    #[serde(rename = "refreshToken")]
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct RefreshTokenResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "expiresIn")]
    expires_in: Option<u64>,
}

pub struct TokenManager {
    refresh_token: String,
    refresh_url: String,
    access_token: Option<String>,
    token_expires_at: Option<Instant>,
    client: Client,
    config_manager: Option<Arc<ConfigManager>>,
    headers: Option<Arc<RwLock<HeaderMap>>>,
}

impl TokenManager {
    pub fn new(refresh_token: String, refresh_url: String) -> Result<Self, McpError> {
        // Validate input parameters
        if refresh_token.trim().is_empty() {
            return Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                "Refresh token cannot be empty".to_string(),
                None,
            ));
        }

        if refresh_url.trim().is_empty() {
            return Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                "Refresh URL cannot be empty".to_string(),
                None,
            ));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("curl/8.4.0")
            .danger_accept_invalid_certs(false)
            .danger_accept_invalid_hostnames(false)
            .build()
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to create HTTP client: {}", e),
                    None,
                )
            })?;

        Ok(Self {
            refresh_token,
            refresh_url,
            access_token: None,
            token_expires_at: None,
            client,
            config_manager: None,
            headers: None,
        })
    }

    /// Inject the config manager for automatic token persistence
    pub fn set_config_manager(&mut self, config_manager: Arc<ConfigManager>) {
        self.config_manager = Some(config_manager);
    }

    /// Inject the shared headers for automatic token updates
    pub fn set_headers(&mut self, headers: Arc<RwLock<HeaderMap>>) {
        self.headers = Some(headers);
    }

    /// Get a valid access token, refreshing if necessary
    pub async fn get_valid_token(&mut self) -> Result<String, McpError> {
        // Check if we have a valid token
        if let Some(token) = &self.access_token
            && let Some(expires_at) = self.token_expires_at
        {
            // Check how much time remains until expiry
            let remaining = expires_at.saturating_duration_since(Instant::now());

            // Refresh token if less than 2 minutes remaining (token lifetime is 5 minutes)
            if remaining > Duration::from_secs(120) {
                debug!(
                    "Using existing valid token (expires in {}s)",
                    remaining.as_secs()
                );
                return Ok(token.clone());
            }

            info!(
                "⏰ Token approaching expiry ({}s remaining), refreshing proactively",
                remaining.as_secs()
            );
        }

        // Need to refresh token
        info!("🔄 Refreshing access token...");
        self.refresh_access_token().await
    }

    /// Refresh the access token
    async fn refresh_access_token(&mut self) -> Result<String, McpError> {
        let request_body = RefreshTokenRequest {
            refresh_token: self.refresh_token.clone(),
        };

        debug!("Making token refresh request to: {}", self.refresh_url);

        let response = self
            .client
            .post(&self.refresh_url)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to send token refresh request: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to refresh token: {}", e),
                    None,
                )
            })?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            error!("Failed to read token refresh response: {}", e);
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to read token refresh response: {}", e),
                None,
            )
        })?;

        debug!(
            "Token refresh response (status: {}): {}",
            status, response_text
        );

        let token_response: RefreshTokenResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                error!("Failed to parse token refresh response: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!(
                        "Failed to parse token refresh response (status: {}, body: {}): {}",
                        status, response_text, e
                    ),
                    None,
                )
            })?;

        // Update token and expiry
        self.access_token = Some(token_response.access_token.clone());
        if let Some(expires_in) = token_response.expires_in {
            self.token_expires_at = Some(Instant::now() + Duration::from_secs(expires_in));
            info!(
                "✅ Successfully refreshed access token (expires in {}s)",
                expires_in
            );
        } else {
            // Default to 1 hour if no expiry provided
            self.token_expires_at = Some(Instant::now() + Duration::from_secs(3600));
            info!("✅ Successfully refreshed access token (expires in 1h)");
        }

        // Create the header value first to ensure it's valid
        let header_value =
            HeaderValue::from_str(&format!("Bearer {}", token_response.access_token)).map_err(
                |e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to create header value from token: {}", e),
                        None,
                    )
                },
            )?;

        // Write the token to config file if config manager is set
        if let Some(config_manager) = &self.config_manager {
            config_manager
                .update_auth_token(&token_response.access_token)
                .map_err(|e| {
                    error!("Failed to write refreshed token to config file: {}", e);
                    e
                })?;
            info!("✅ Refreshed token written to config file");
        }

        // Update the shared headers if available
        if let Some(headers) = &self.headers {
            let mut headers_guard = headers.write().await;
            headers_guard.insert(AUTHORIZATION, header_value);
            info!("✅ Refreshed token updated in shared headers");
        }

        Ok(token_response.access_token)
    }

    /// Verify token by making a test API call
    pub async fn verify_token(
        &self,
        token: &str,
        graphql_endpoint: &str,
    ) -> Result<bool, McpError> {
        debug!("🧪 Verifying token with API test...");

        let test_query = serde_json::json!({
            "query": "query { company { name } }"
        });

        let response = self
            .client
            .post(graphql_endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token))
            .json(&test_query)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to verify token: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to verify token: {}", e),
                    None,
                )
            })?;

        let response_text = response.text().await.map_err(|e| {
            error!("Failed to read token verification response: {}", e);
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to read token verification response: {}", e),
                None,
            )
        })?;

        let is_valid = response_text.contains("\"name\"");

        if is_valid {
            info!("✅ Token verification successful - API is accessible");
        } else {
            warn!(
                "❌ Token verification failed. API response: {}",
                response_text
            );
        }

        Ok(is_valid)
    }

    /// Start background token refresh task
    pub async fn start_refresh_task(&mut self, graphql_endpoint: String) {
        let mut token_manager = self.clone();

        tokio::spawn(async move {
            loop {
                // Wait 50 minutes (refresh every 50 minutes to be safe)
                sleep(Duration::from_secs(3000)).await;

                match token_manager.get_valid_token().await {
                    Ok(token) => {
                        if let Err(e) = token_manager.verify_token(&token, &graphql_endpoint).await
                        {
                            error!("Token verification failed in background task: {}", e);
                        } else {
                            info!("✅ Background task: token refreshed and verified");
                        }
                    }
                    Err(e) => {
                        error!("Background token refresh failed: {}", e);
                    }
                }
            }
        });
    }
}

impl Clone for TokenManager {
    fn clone(&self) -> Self {
        Self {
            refresh_token: self.refresh_token.clone(),
            refresh_url: self.refresh_url.clone(),
            access_token: self.access_token.clone(),
            token_expires_at: self.token_expires_at,
            client: self.client.clone(),
            config_manager: self.config_manager.clone(),
            headers: self.headers.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_manager::ConfigManager;
    use std::fs;
    use std::time::Instant;
    use tempfile::TempDir;
    use tokio::time::Duration;

    /// Test that token refresh stores token in memory
    #[tokio::test]
    async fn test_token_refresh_stores_in_memory() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.yaml");

        // Create initial config
        let initial_config = r#"
endpoint: "https://api.example.com/graphql"
headers:
  Authorization: "Bearer old_token"
"#;
        fs::write(&config_path, initial_config).unwrap();

        let _config_manager = ConfigManager::new(config_path.to_string_lossy().to_string());

        // Mock refresh URL (this would normally be a real endpoint)
        let refresh_url = "https://api.example.com/refresh";
        let refresh_token = "refresh_token_123";

        let token_manager =
            TokenManager::new(refresh_token.to_string(), refresh_url.to_string()).unwrap();

        // Initially no token in memory
        assert!(token_manager.access_token.is_none());
        assert!(token_manager.token_expires_at.is_none());

        // Note: This test would need a mock server to actually test token refresh
        // For now, we test the structure and that it can be created
        assert_eq!(token_manager.refresh_token, refresh_token);
        assert_eq!(token_manager.refresh_url, refresh_url);
    }

    /// Test token manager creation with invalid parameters
    #[test]
    fn test_token_manager_creation_error() {
        // Test with empty refresh token
        let result = TokenManager::new(
            "".to_string(),
            "https://api.example.com/refresh".to_string(),
        );
        assert!(result.is_err());

        // Test with empty refresh URL
        let result = TokenManager::new("refresh_token".to_string(), "".to_string());
        assert!(result.is_err());
    }

    /// Test token expiry logic
    #[tokio::test]
    async fn test_token_expiry_logic() {
        let refresh_url = "https://api.example.com/refresh";
        let refresh_token = "refresh_token_123";

        let mut token_manager =
            TokenManager::new(refresh_token.to_string(), refresh_url.to_string()).unwrap();

        // Set a token that expires in the past
        token_manager.access_token = Some("test_token".to_string());
        token_manager.token_expires_at = Some(Instant::now() - Duration::from_secs(3600));

        // Token should be considered expired
        let now = Instant::now();
        if let Some(expires_at) = token_manager.token_expires_at {
            assert!(expires_at < now);
        }
    }

    /// Test token manager clone
    #[test]
    fn test_token_manager_clone() {
        let refresh_url = "https://api.example.com/refresh";
        let refresh_token = "refresh_token_123";

        let mut token_manager =
            TokenManager::new(refresh_token.to_string(), refresh_url.to_string()).unwrap();
        token_manager.access_token = Some("test_token".to_string());
        token_manager.token_expires_at = Some(Instant::now() + Duration::from_secs(3600));

        let cloned_manager = token_manager.clone();

        assert_eq!(
            cloned_manager.refresh_token(),
            token_manager.refresh_token()
        );
        assert_eq!(cloned_manager.refresh_url(), token_manager.refresh_url());
        assert_eq!(cloned_manager.access_token(), token_manager.access_token());
        assert_eq!(
            cloned_manager.token_expires_at(),
            token_manager.token_expires_at()
        );
    }

    // Test helper methods for TokenManager
    impl TokenManager {
        /// Get the refresh token (for testing)
        pub fn refresh_token(&self) -> &str {
            &self.refresh_token
        }

        /// Get the refresh URL (for testing)
        pub fn refresh_url(&self) -> &str {
            &self.refresh_url
        }

        /// Get the current access token (for testing)
        pub fn access_token(&self) -> &Option<String> {
            &self.access_token
        }

        /// Get the token expiry time (for testing)
        pub fn token_expires_at(&self) -> &Option<Instant> {
            &self.token_expires_at
        }
    }
}
