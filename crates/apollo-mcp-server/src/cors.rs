use http::Method;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use url::Url;

use crate::errors::ServerError;

/// CORS configuration options
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CorsConfig {
    /// Enable CORS support
    pub enabled: bool,

    /// List of allowed origins (exact match)
    pub origins: Vec<String>,

    /// List of origin patterns (regex matching)
    pub match_origins: Vec<String>,

    /// Allow any origin (use with caution)
    pub allow_any_origin: bool,

    /// Allow credentials in CORS requests
    pub allow_credentials: bool,

    /// Allowed HTTP methods
    pub allow_methods: Vec<String>,

    /// Allowed request headers
    pub allow_headers: Vec<String>,

    /// Headers exposed to the browser
    pub expose_headers: Vec<String>,

    /// Max age for preflight cache (in seconds)
    pub max_age: Option<u64>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            origins: Vec::new(),
            match_origins: Vec::new(),
            allow_any_origin: false,
            allow_credentials: false,
            allow_methods: default_methods(),
            allow_headers: default_headers(),
            expose_headers: Vec::new(),
            max_age: Some(default_max_age()),
        }
    }
}

/// Default allowed HTTP methods
fn default_methods() -> Vec<String> {
    vec!["GET".to_string(), "POST".to_string(), "OPTIONS".to_string()]
}

/// Default allowed headers
fn default_headers() -> Vec<String> {
    vec![
        "content-type".to_string(),
        "authorization".to_string(),
        "mcp-session-id".to_string(),
    ]
}

/// Default max age for preflight cache (2 hours)
fn default_max_age() -> u64 {
    7200
}

impl CorsConfig {
    /// Build a CorsLayer from this configuration
    pub fn build_cors_layer(&self) -> Result<CorsLayer, ServerError> {
        if !self.enabled {
            return Err(ServerError::Cors("CORS is not enabled".to_string()));
        }

        // Validate configuration
        self.validate()?;

        let mut cors = CorsLayer::new();

        // Configure origins
        if self.allow_any_origin {
            cors = cors.allow_origin(Any);
        } else {
            // Collect all origins (exact and regex patterns)
            let mut origin_list = Vec::new();

            // Parse exact origins
            for origin_str in &self.origins {
                let origin = origin_str.parse::<http::HeaderValue>().map_err(|e| {
                    ServerError::Cors(format!("Invalid origin '{}': {}", origin_str, e))
                })?;
                origin_list.push(origin);
            }

            // For regex patterns, we need to use a predicate function
            if !self.match_origins.is_empty() {
                // Parse regex patterns to validate them
                let mut regex_patterns = Vec::new();
                for pattern in &self.match_origins {
                    let regex = Regex::new(pattern).map_err(|e| {
                        ServerError::Cors(format!("Invalid origin pattern '{}': {}", pattern, e))
                    })?;
                    regex_patterns.push(regex);
                }

                // Use predicate function that combines exact origins and regex patterns
                let exact_origins = origin_list;
                cors = cors.allow_origin(AllowOrigin::predicate(move |origin, _| {
                    let origin_str = origin.to_str().unwrap_or("");

                    // Check exact origins
                    if exact_origins
                        .iter()
                        .any(|exact| exact.as_bytes() == origin.as_bytes())
                    {
                        return true;
                    }

                    // Check regex patterns
                    regex_patterns
                        .iter()
                        .any(|regex| regex.is_match(origin_str))
                }));
            } else if !origin_list.is_empty() {
                // Only exact origins, no regex
                cors = cors.allow_origin(origin_list);
            }
        }

        // Configure credentials
        cors = cors.allow_credentials(self.allow_credentials);

        // Configure methods
        let methods: Result<Vec<Method>, _> = self
            .allow_methods
            .iter()
            .map(|m| m.parse::<Method>())
            .collect();
        let methods =
            methods.map_err(|e| ServerError::Cors(format!("Invalid HTTP method: {}", e)))?;
        cors = cors.allow_methods(methods);

        // Configure headers
        if !self.allow_headers.is_empty() {
            let headers: Result<Vec<http::HeaderName>, _> = self
                .allow_headers
                .iter()
                .map(|h| h.parse::<http::HeaderName>())
                .collect();
            let headers =
                headers.map_err(|e| ServerError::Cors(format!("Invalid header name: {}", e)))?;
            cors = cors.allow_headers(headers);
        }

        // Configure exposed headers
        if !self.expose_headers.is_empty() {
            let headers: Result<Vec<http::HeaderName>, _> = self
                .expose_headers
                .iter()
                .map(|h| h.parse::<http::HeaderName>())
                .collect();
            let headers = headers
                .map_err(|e| ServerError::Cors(format!("Invalid exposed header name: {}", e)))?;
            cors = cors.expose_headers(headers);
        }

        // Configure max age
        if let Some(max_age) = self.max_age {
            cors = cors.max_age(std::time::Duration::from_secs(max_age));
        }

        Ok(cors)
    }

    /// Validate the configuration for consistency
    fn validate(&self) -> Result<(), ServerError> {
        // Cannot use credentials with any origin
        if self.allow_credentials && self.allow_any_origin {
            return Err(ServerError::Cors(
                "Cannot use allow_credentials with allow_any_origin for security reasons"
                    .to_string(),
            ));
        }

        // Must have at least some origin configuration if not allowing any origin
        if !self.allow_any_origin && self.origins.is_empty() && self.match_origins.is_empty() {
            return Err(ServerError::Cors(
                "Must specify origins, match_origins, or allow_any_origin when CORS is enabled"
                    .to_string(),
            ));
        }

        // Validate that origin strings are valid URLs
        for origin in &self.origins {
            Url::parse(origin).map_err(|e| {
                ServerError::Cors(format!("Invalid origin URL '{}': {}", origin, e))
            })?;
        }

        // Validate regex patterns
        for pattern in &self.match_origins {
            Regex::new(pattern).map_err(|e| {
                ServerError::Cors(format!("Invalid regex pattern '{}': {}", pattern, e))
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CorsConfig::default();
        assert!(!config.enabled);
        assert!(!config.allow_any_origin);
        assert!(!config.allow_credentials);
        assert_eq!(config.allow_methods, default_methods());
        assert_eq!(config.allow_headers, default_headers());
        assert_eq!(config.max_age, Some(default_max_age()));
    }

    #[test]
    fn test_disabled_cors_fails_to_build() {
        let config = CorsConfig::default();
        assert!(config.build_cors_layer().is_err());
    }

    #[test]
    fn test_allow_any_origin_builds() {
        let config = CorsConfig {
            enabled: true,
            allow_any_origin: true,
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_ok());
    }

    #[test]
    fn test_specific_origins_build() {
        let config = CorsConfig {
            enabled: true,
            origins: vec![
                "https://localhost:3000".to_string(),
                "https://studio.apollographql.com".to_string(),
            ],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_ok());
    }

    #[test]
    fn test_regex_origins_build() {
        let config = CorsConfig {
            enabled: true,
            match_origins: vec!["^https://localhost:[0-9]+$".to_string()],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_ok());
    }

    #[test]
    fn test_credentials_with_any_origin_fails() {
        let config = CorsConfig {
            enabled: true,
            allow_any_origin: true,
            allow_credentials: true,
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_err());
    }

    #[test]
    fn test_no_origins_fails() {
        let config = CorsConfig {
            enabled: true,
            allow_any_origin: false,
            origins: vec![],
            match_origins: vec![],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_err());
    }

    #[test]
    fn test_invalid_origin_fails() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["not-a-valid-url".to_string()],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_err());
    }

    #[test]
    fn test_invalid_regex_fails() {
        let config = CorsConfig {
            enabled: true,
            match_origins: vec!["[invalid regex".to_string()],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_err());
    }

    #[test]
    fn test_invalid_method_fails() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://localhost:3000".to_string()],
            allow_methods: vec!["invalid method with spaces".to_string()],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_err());
    }
}
