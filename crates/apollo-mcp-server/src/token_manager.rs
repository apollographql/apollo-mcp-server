//! Token refresh functionality for Apollo MCP Server

use crate::errors::McpError;
use reqwest::Client;
use rmcp::model::ErrorCode;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

#[derive(Debug, Serialize)]
struct RefreshTokenRequest {
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

pub struct TokenManager {
    refresh_token: String,
    refresh_url: String,
    access_token: Option<String>,
    token_expires_at: Option<Instant>,
    client: Client,
}

impl TokenManager {
    pub fn new(refresh_token: String, refresh_url: String) -> Self {
        Self {
            refresh_token,
            refresh_url,
            access_token: None,
            token_expires_at: None,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10))
                .user_agent("curl/8.4.0")
                .danger_accept_invalid_certs(false)
                .danger_accept_invalid_hostnames(false)
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Get a valid access token, refreshing if necessary
    pub async fn get_valid_token(&mut self) -> Result<String, McpError> {
        // Check if we have a valid token
        if let Some(token) = &self.access_token {
            if let Some(expires_at) = self.token_expires_at {
                // Refresh token 5 minutes before expiry
                if expires_at.duration_since(Instant::now()) > Duration::from_secs(300) {
                    debug!("Using existing valid token");
                    return Ok(token.clone());
                }
            }
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

        let response = self.client
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

        debug!("Token refresh response (status: {}): {}", status, response_text);

        let token_response: RefreshTokenResponse = serde_json::from_str(&response_text)
            .map_err(|e| {
                error!("Failed to parse token refresh response: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to parse token refresh response (status: {}, body: {}): {}", 
                           status, response_text, e),
                    None,
                )
            })?;

        // Update token and expiry
        self.access_token = Some(token_response.access_token.clone());
        if let Some(expires_in) = token_response.expires_in {
            self.token_expires_at = Some(Instant::now() + Duration::from_secs(expires_in));
            info!("✅ Successfully refreshed access token (expires in {}s)", expires_in);
        } else {
            // Default to 1 hour if no expiry provided
            self.token_expires_at = Some(Instant::now() + Duration::from_secs(3600));
            info!("✅ Successfully refreshed access token (expires in 1h)");
        }

        Ok(token_response.access_token)
    }

    /// Verify token by making a test API call
    pub async fn verify_token(&self, token: &str, graphql_endpoint: &str) -> Result<bool, McpError> {
        debug!("🧪 Verifying token with API test...");

        let test_query = serde_json::json!({
            "query": "query { company { name } }"
        });

        let response = self.client
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
            warn!("❌ Token verification failed. API response: {}", response_text);
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
                        if let Err(e) = token_manager.verify_token(&token, &graphql_endpoint).await {
                            error!("Token verification failed in background task: {}", e);
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
        }
    }
}
