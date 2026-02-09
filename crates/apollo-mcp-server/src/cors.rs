use http::Method;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use url::Url;

use crate::errors::ServerError;

/// CORS configuration options
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
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
            allow_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "DELETE".to_string(), // Clients that no longer need a particular session SHOULD send an HTTP DELETE to explicitly terminate the session
            ],
            allow_headers: vec![
                "content-type".to_string(),
                "mcp-protocol-version".to_string(), // https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#protocol-version-header
                "mcp-session-id".to_string(), // https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#session-management
                "traceparent".to_string(), // https://www.w3.org/TR/trace-context/#traceparent-header
                "tracestate".to_string(),  // https://www.w3.org/TR/trace-context/#tracestate-header
            ],
            expose_headers: vec![
                "mcp-session-id".to_string(), // https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#session-management
                "traceparent".to_string(), // https://www.w3.org/TR/trace-context/#traceparent-header
                "tracestate".to_string(),  // https://www.w3.org/TR/trace-context/#tracestate-header
            ],
            max_age: Some(7200), // 2 hours
        }
    }
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
                "Cannot use allow_credentials with allow_any_origin for security reasons. See: https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/CORS#requests_with_credentials"
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
    use axum::{Router, routing::get};
    use http::{HeaderValue, Method, Request, StatusCode};
    use tower::util::ServiceExt;

    #[test]
    fn test_default_config() {
        let config = CorsConfig::default();
        assert!(!config.enabled);
        assert!(!config.allow_any_origin);
        assert!(!config.allow_credentials);
        assert_eq!(
            config.allow_methods,
            vec!["GET".to_string(), "POST".to_string(), "DELETE".to_string()]
        );
        assert_eq!(
            config.allow_headers,
            vec![
                "content-type".to_string(),
                "mcp-protocol-version".to_string(),
                "mcp-session-id".to_string(),
                "traceparent".to_string(),
                "tracestate".to_string(),
            ]
        );
        assert_eq!(
            config.expose_headers,
            vec![
                "mcp-session-id".to_string(),
                "traceparent".to_string(),
                "tracestate".to_string(),
            ]
        );
        assert_eq!(config.max_age, Some(7200));
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
                "http://localhost:3000".to_string(),
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
            match_origins: vec!["^http://localhost:[0-9]+$".to_string()],
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
            origins: vec!["http://localhost:3000".to_string()],
            allow_methods: vec!["invalid method with spaces".to_string()],
            ..Default::default()
        };
        assert!(config.build_cors_layer().is_err());
    }

    #[tokio::test]
    async fn test_preflight_request_with_exact_origin() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["http://localhost:3000".to_string()],
            max_age: Some(3600),
            ..Default::default()
        };

        let app = Router::new().layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "http://localhost:3000")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("http://localhost:3000"))
        );
    }

    #[tokio::test]
    async fn test_simple_request_with_exact_origin() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["http://localhost:3000".to_string()],
            ..Default::default()
        };

        let app = Router::new()
            .route("/health", get(|| async { "test response" }))
            .layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::GET)
            .uri("/health")
            .header("Origin", "http://localhost:3000")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("http://localhost:3000"))
        );
    }

    #[tokio::test]
    async fn test_preflight_request_with_regex_origin() {
        let config = CorsConfig {
            enabled: true,
            match_origins: vec!["^http://localhost:[0-9]+$".to_string()],
            ..Default::default()
        };

        let app = Router::new().layer(config.build_cors_layer().unwrap());

        // Test matching port
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "http://localhost:4321")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("http://localhost:4321"))
        );
    }

    #[tokio::test]
    async fn test_simple_request_with_regex_origin() {
        let config = CorsConfig {
            enabled: true,
            match_origins: vec!["^https://.*\\.apollographql\\.com$".to_string()],
            ..Default::default()
        };

        let app = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "https://www.apollographql.com")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("https://www.apollographql.com"))
        );
    }

    #[tokio::test]
    async fn test_mixed_exact_and_regex_origins() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["http://localhost:3000".to_string()],
            match_origins: vec!["^https://.*\\.apollographql\\.com$".to_string()],
            ..Default::default()
        };

        let cors_layer = config.build_cors_layer().unwrap();

        // Test exact origin
        let app1 = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(cors_layer.clone());

        let request1 = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "http://localhost:3000")
            .body(axum::body::Body::empty())
            .unwrap();

        let response1 = app1.oneshot(request1).await.unwrap();
        assert_eq!(
            response1.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("http://localhost:3000"))
        );

        // Test regex origin
        let app2 = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(cors_layer);

        let request2 = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "https://studio.apollographql.com")
            .body(axum::body::Body::empty())
            .unwrap();

        let response2 = app2.oneshot(request2).await.unwrap();
        assert_eq!(
            response2.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static(
                "https://studio.apollographql.com"
            ))
        );
    }

    #[tokio::test]
    async fn test_preflight_request_rejected_origin_exact() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://allowed.com".to_string()],
            ..Default::default()
        };

        let app = Router::new().layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "https://blocked.com")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_simple_request_rejected_origin_exact() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://allowed.com".to_string()],
            ..Default::default()
        };

        let app = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "https://blocked.com")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_preflight_request_rejected_origin_regex() {
        let config = CorsConfig {
            enabled: true,
            match_origins: vec!["^https://.*\\.allowed\\.com$".to_string()],
            ..Default::default()
        };

        let cors_layer = config.build_cors_layer().unwrap();
        let app = Router::new().layer(cors_layer);

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "https://malicious.blocked.com")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_simple_request_rejected_origin_regex() {
        let config = CorsConfig {
            enabled: true,
            match_origins: vec!["^https://.*\\.allowed\\.com$".to_string()],
            ..Default::default()
        };

        let cors_layer = config.build_cors_layer().unwrap();
        let app = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(cors_layer);

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "https://malicious.blocked.com")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_preflight_request_any_origin() {
        let config = CorsConfig {
            enabled: true,
            allow_any_origin: true,
            ..Default::default()
        };

        let app = Router::new().layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "https://any-domain.com")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("*"))
        );
    }

    #[tokio::test]
    async fn test_simple_request_any_origin() {
        let config = CorsConfig {
            enabled: true,
            allow_any_origin: true,
            ..Default::default()
        };

        let app = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "https://any-domain.com")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-origin"),
            Some(&HeaderValue::from_static("*"))
        );
    }

    #[tokio::test]
    async fn test_non_cors_request() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://allowed.com".to_string()],
            ..Default::default()
        };

        let cors_layer = config.build_cors_layer().unwrap();
        let app = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(cors_layer);

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            // No Origin header
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Request should succeed but without CORS headers
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_multiple_request_headers() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://allowed.com".to_string()],
            allow_headers: vec![
                "content-type".to_string(),
                "authorization".to_string(),
                "x-api-key".to_string(),
                "x-requested-with".to_string(),
            ],
            ..Default::default()
        };

        let app = Router::new().layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "https://allowed.com")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization,x-api-key,disallowed-header",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let allow_headers = response
            .headers()
            .get("access-control-allow-headers")
            .unwrap();
        let headers_str = allow_headers.to_str().unwrap();
        assert!(headers_str.contains("content-type"));
        assert!(headers_str.contains("authorization"));
        assert!(headers_str.contains("x-api-key"));
        assert!(!headers_str.contains("disallowed-header"));
    }

    #[tokio::test]
    async fn test_preflight_request_with_credentials() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://allowed.com".to_string()],
            allow_credentials: true,
            ..Default::default()
        };

        let app = Router::new().layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/test")
            .header("Origin", "https://allowed.com")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,authorization",
            )
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-credentials"),
            Some(&HeaderValue::from_static("true"))
        );
    }

    #[tokio::test]
    async fn test_simple_request_with_credentials() {
        let config = CorsConfig {
            enabled: true,
            origins: vec!["https://allowed.com".to_string()],
            allow_credentials: true,
            ..Default::default()
        };

        let app = Router::new()
            .route("/test", get(|| async { "test response" }))
            .layer(config.build_cors_layer().unwrap());

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("Origin", "https://allowed.com")
            .header("Cookie", "sessionid=abc123")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("access-control-allow-credentials"),
            Some(&HeaderValue::from_static("true"))
        );
    }
}
