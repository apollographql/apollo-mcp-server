//! Configuration file management for Apollo MCP Server

use crate::errors::McpError;
use rmcp::model::ErrorCode;
use std::fs;
use std::path::Path;
use tracing::{debug, error, info, warn};

pub struct ConfigManager {
    config_path: String,
}

impl ConfigManager {
    pub fn new(config_path: String) -> Self {
        Self { config_path }
    }

    /// Update the authorization token in the config file
    pub fn update_auth_token(&self, new_token: &str) -> Result<(), McpError> {
        info!("🔧 Updating config file with new token...");

        // Create backup
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let backup_path = format!("{}.backup.{}", self.config_path, timestamp);
        
        if let Err(e) = fs::copy(&self.config_path, &backup_path) {
            warn!("Failed to create backup: {}", e);
        } else {
            info!("💾 Backup created: {}", backup_path);
        }

        // Read current config
        let config_content = fs::read_to_string(&self.config_path)
            .map_err(|e| {
                error!("Failed to read config file: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to read config file: {}", e),
                    None,
                )
            })?;

        // Update authorization header
        let updated_content = config_content
            .lines()
            .map(|line| {
                if line.contains("Authorization: Bearer") {
                    format!("Authorization: Bearer {}", new_token)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Write updated config
        fs::write(&self.config_path, updated_content)
            .map_err(|e| {
                error!("Failed to write updated config file: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to write updated config file: {}", e),
                    None,
                )
            })?;

        info!("✅ Config file updated with new token");
        Ok(())
    }

    /// Read the current authorization token from config file
    pub fn get_current_token(&self) -> Result<Option<String>, McpError> {
        let config_content = fs::read_to_string(&self.config_path)
            .map_err(|e| {
                error!("Failed to read config file: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to read config file: {}", e),
                    None,
                )
            })?;

        for line in config_content.lines() {
            if line.contains("Authorization: Bearer") {
                if let Some(token) = line.split("Bearer ").nth(1) {
                    return Ok(Some(token.trim().to_string()));
                }
            }
        }

        Ok(None)
    }

    /// Verify config file exists and is readable
    pub fn verify_config(&self) -> Result<(), McpError> {
        if !Path::new(&self.config_path).exists() {
            return Err(McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Config file does not exist: {}", self.config_path),
                None,
            ));
        }

        fs::read_to_string(&self.config_path)
            .map_err(|e| {
                error!("Config file is not readable: {}", e);
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Config file is not readable: {}", e),
                    None,
                )
            })?;

        debug!("Config file verified: {}", self.config_path);
        Ok(())
    }
}
