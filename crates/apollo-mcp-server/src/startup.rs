//! Startup and initialization functions for Apollo MCP Server

use crate::config_manager::ConfigManager;
use crate::errors::McpError;
use crate::token_manager::TokenManager;
use rmcp::model::ErrorCode;
use std::env;
use tracing::{debug, info, warn};

/// Initialize the Apollo MCP Server with token refresh and environment setup
pub async fn initialize_with_token_refresh(
    config_path: String,
    refresh_token: String,
    refresh_url: String,
    graphql_endpoint: String,
) -> Result<(), McpError> {
    info!("🎯 Apollo MCP Server initializing with token refresh...");

    // Step 1: Verify config file
    let config_manager = ConfigManager::new(config_path.clone());
    config_manager.verify_config().map_err(|e| {
        warn!("Config verification failed: {}", e);
        e
    })?;

    // Step 2: Initialize token manager
    let mut token_manager = TokenManager::new(refresh_token, refresh_url);

    // Step 3: Get fresh token
    let new_token = token_manager.get_valid_token().await.map_err(|e| {
        warn!("Token refresh failed: {}", e);
        e
    })?;

    // Step 4: Update config file with new token
    config_manager.update_auth_token(&new_token).map_err(|e| {
        warn!("Config update failed: {}", e);
        e
    })?;

    // Step 5: Verify the new token
    if !token_manager.verify_token(&new_token, &graphql_endpoint).await.map_err(|e| {
        warn!("Token verification failed: {}", e);
        e
    })? {
        return Err(McpError::new(
            ErrorCode::INTERNAL_ERROR,
            "Token verification failed after refresh".to_string(),
            None,
        ));
    }

    // Step 6: Start background token refresh task
    token_manager.start_refresh_task(graphql_endpoint).await;

    // Step 7: Set up environment variables
    setup_environment_variables();

    info!("✅ Apollo MCP Server initialization complete");
    Ok(())
}

/// Set up environment variables for optimal HTTP client behavior
fn setup_environment_variables() {
    info!("🔧 Setting up environment variables...");

    unsafe {
        // Set environment variables to match curl behavior
        env::set_var("RUSTLS_SYSTEM_CERT_ROOT", "1"); // Use system certificates like curl
        env::set_var("HTTP_PROXY", ""); // Disable proxy
        env::set_var("HTTPS_PROXY", ""); // Disable HTTPS proxy
        env::set_var("NO_PROXY", ""); // Clear no-proxy list
        env::set_var("RUST_LOG", "error"); // Set logging level to error

        // Set reqwest-specific environment variables
        env::set_var("REQWEST_TIMEOUT", "30"); // 30 second timeout
        env::set_var("REQWEST_CONNECT_TIMEOUT", "10"); // 10 second connect timeout
        env::set_var("REQWEST_USER_AGENT", "curl/8.4.0"); // Match curl user agent
        env::set_var("REQWEST_SSL_VERIFY", "true"); // Enable SSL verification
        env::set_var("REQWEST_SSL_VERIFY_HOSTNAME", "true"); // Enable hostname verification
    }

    debug!("Environment variables set:");
    debug!("   RUSTLS_SYSTEM_CERT_ROOT=1");
    debug!("   HTTP_PROXY=");
    debug!("   HTTPS_PROXY=");
    debug!("   RUST_LOG=error");
    debug!("   REQWEST_TIMEOUT=30");
    debug!("   REQWEST_CONNECT_TIMEOUT=10");
    debug!("   REQWEST_USER_AGENT=curl/8.4.0");
    debug!("   REQWEST_SSL_VERIFY=true");
    debug!("   REQWEST_SSL_VERIFY_HOSTNAME=true");
}

/// Check if token refresh is enabled via environment variables
pub fn is_token_refresh_enabled() -> bool {
    env::var("APOLLO_TOKEN_REFRESH_ENABLED")
        .ok()
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Get refresh token from environment
pub fn get_refresh_token() -> Option<String> {
    env::var("APOLLO_REFRESH_TOKEN").ok()
}

/// Get refresh URL from environment
pub fn get_refresh_url() -> Option<String> {
    env::var("APOLLO_REFRESH_URL").ok()
}

/// Get GraphQL endpoint from environment
pub fn get_graphql_endpoint() -> Option<String> {
    env::var("APOLLO_GRAPHQL_ENDPOINT").ok()
}
