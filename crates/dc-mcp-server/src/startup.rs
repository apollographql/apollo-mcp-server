//! Startup and initialization functions for Apollo MCP Server

use crate::config_manager::ConfigManager;
use crate::errors::McpError;
use crate::token_manager::TokenManager;
use reqwest::header::HeaderMap;
use std::env;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Create and configure a TokenManager for on-demand token refresh
/// Returns the TokenManager which will refresh tokens when needed before requests
pub fn create_token_manager(
    config_path: String,
    refresh_token: String,
    refresh_url: String,
    _graphql_endpoint: String,
    shared_headers: Arc<RwLock<HeaderMap>>,
) -> Result<TokenManager, McpError> {
    info!("🎯 Apollo MCP Server initializing with token refresh...");
    info!("📝 Config path: {}", config_path);
    info!("🔗 Refresh URL: {}", refresh_url);

    // Step 1: Create shared config manager
    info!("Step 1: Creating config manager...");
    let config_manager = Arc::new(ConfigManager::new(config_path.clone()));

    info!("Step 1a: Verifying config...");
    config_manager.verify_config().map_err(|e| {
        warn!("Config verification failed: {}", e);
        e
    })?;
    info!("✅ Config verified");

    // Step 2: Initialize token manager with injected config manager and headers
    info!("Step 2: Creating token manager...");
    let mut token_manager = TokenManager::new(refresh_token, refresh_url)?;
    info!("✅ Token manager created");

    info!("Step 2a: Setting config manager...");
    token_manager.set_config_manager(Arc::clone(&config_manager));
    info!("✅ Config manager set");

    info!("Step 2b: Setting headers...");
    token_manager.set_headers(Arc::clone(&shared_headers));
    info!("✅ Headers set");

    info!("✅ Apollo MCP Server token manager ready for on-demand refresh");
    Ok(token_manager)
}

/// Check if token refresh is enabled via environment variables
pub fn is_token_refresh_enabled() -> bool {
    env::var("DC_TOKEN_REFRESH_ENABLED")
        .ok()
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Get refresh token from environment
pub fn get_refresh_token() -> Option<String> {
    env::var("DC_REFRESH_TOKEN").ok()
}

/// Get refresh URL from environment
pub fn get_refresh_url() -> Option<String> {
    env::var("DC_REFRESH_URL").ok()
}

/// Get GraphQL endpoint from environment
pub fn get_graphql_endpoint() -> Option<String> {
    env::var("DC_GRAPHQL_ENDPOINT").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_manager::ConfigManager;
    use crate::token_manager::TokenManager;
    use std::fs;
    use tempfile::TempDir;

    /// Test complete initialization flow
    #[tokio::test]
    async fn test_complete_initialization_flow() {
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

        // Test environment variable setup
        unsafe {
            std::env::set_var("DC_REFRESH_TOKEN", "test_refresh_token");
            std::env::set_var("DC_REFRESH_URL", "https://api.example.com/refresh");
            std::env::set_var("DC_GRAPHQL_ENDPOINT", "https://api.example.com/graphql");
        }

        // Test getting refresh token from environment
        let refresh_token = get_refresh_token();
        assert_eq!(refresh_token, Some("test_refresh_token".to_string()));

        // Test getting refresh URL from environment
        let refresh_url = get_refresh_url();
        assert_eq!(
            refresh_url,
            Some("https://api.example.com/refresh".to_string())
        );

        // Test getting GraphQL endpoint from environment
        let endpoint = get_graphql_endpoint();
        assert_eq!(
            endpoint,
            Some("https://api.example.com/graphql".to_string())
        );

        // Clean up environment variables
        unsafe {
            std::env::remove_var("DC_REFRESH_TOKEN");
            std::env::remove_var("DC_REFRESH_URL");
            std::env::remove_var("DC_GRAPHQL_ENDPOINT");
        }
    }

    /// Test initialization with missing environment variables
    #[tokio::test]
    async fn test_initialization_missing_env_vars() {
        // Ensure environment variables are not set
        unsafe {
            std::env::remove_var("DC_REFRESH_TOKEN");
            std::env::remove_var("DC_REFRESH_URL");
        }

        // Test getting missing refresh token
        let refresh_token = get_refresh_token();
        assert_eq!(refresh_token, None);

        // Test getting missing refresh URL
        let refresh_url = get_refresh_url();
        assert_eq!(refresh_url, None);
    }

    /// Test token manager integration
    #[tokio::test]
    async fn test_token_manager_integration() {
        let refresh_token = "test_refresh_token";
        let refresh_url = "https://api.example.com/refresh";

        // Test creating token manager
        let token_manager = TokenManager::new(refresh_token.to_string(), refresh_url.to_string());
        assert!(token_manager.is_ok());

        let token_manager = token_manager.unwrap();
        assert_eq!(token_manager.refresh_token(), refresh_token);
        assert_eq!(token_manager.refresh_url(), refresh_url);
    }
}
