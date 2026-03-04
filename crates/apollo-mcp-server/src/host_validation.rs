use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::HOST},
    middleware::Next,
    response::{IntoResponse, Response},
};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::warn;

/// Configuration for Host header validation to prevent DNS rebinding attacks.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HostValidationConfig {
    /// Enable Host header validation (enabled by default for security)
    pub enabled: bool,

    /// Additional allowed hosts beyond localhost, 127.0.0.1, ::1, and 0.0.0.0.
    pub allowed_hosts: Vec<String>,
}

impl Default for HostValidationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_hosts: Vec::new(),
        }
    }
}

impl HostValidationConfig {
    /// Creates a configuration with Host header validation disabled.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            allowed_hosts: Vec::new(),
        }
    }
}

/// State for the Host header validation middleware.
#[derive(Clone)]
pub struct HostValidationState {
    /// The validation configuration (wrapped in Arc to avoid cloning Vec on each request).
    pub config: Arc<HostValidationConfig>,
    /// The port the server is listening on, used to validate localhost requests.
    pub server_port: u16,
}

impl HostValidationState {
    fn is_host_allowed(&self, host: &str) -> bool {
        if !self.config.enabled {
            return true;
        }

        let hostname = host
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host)
            .trim_start_matches('[')
            .trim_end_matches(']');

        // Check if hostname is localhost: literal "localhost", loopback (127.0.0.1, ::1), or unspecified (0.0.0.0, ::)
        let is_localhost = hostname.eq_ignore_ascii_case("localhost")
            || hostname
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback() || ip.is_unspecified())
                .unwrap_or(false);

        // Localhost: validate port against actual server port
        if is_localhost {
            if let Some(port_str) = host.rsplit_once(':').map(|(_, p)| p) {
                if let Ok(port) = port_str.parse::<u16>() {
                    return port == self.server_port;
                }
                return false;
            }
            return true;
        }

        // Custom hosts: validate port against config (if specified).
        // No port in config means any port is allowed for flexibility with proxies.
        for allowed in &self.config.allowed_hosts {
            let allowed_hostname = allowed.rsplit_once(':').map(|(h, _)| h).unwrap_or(allowed);

            if hostname.eq_ignore_ascii_case(allowed_hostname) {
                if let Some(allowed_port_str) = allowed.rsplit_once(':').map(|(_, p)| p) {
                    if let Some(host_port_str) = host.rsplit_once(':').map(|(_, p)| p) {
                        return allowed_port_str == host_port_str;
                    }
                    return false;
                }
                return true;
            }
        }

        false
    }
}

/// Middleware that validates the Host header to prevent DNS rebinding attacks.
pub async fn validate_host(
    State(state): State<HostValidationState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.config.enabled {
        return next.run(request).await;
    }

    // Extract host from Host header (HTTP/1.1) or URI authority (HTTP/2).
    // Use Cow to avoid allocation when Host header is present (common case).
    let host: Option<Cow<'_, str>> = request
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(Cow::Borrowed)
        .or_else(|| {
            request.uri().host().map(|h| {
                // Include port from URI if present (requires allocation)
                match request.uri().port_u16() {
                    Some(port) => Cow::Owned(format!("{}:{}", h, port)),
                    None => Cow::Borrowed(h),
                }
            })
        });

    match host {
        Some(host) => {
            if state.is_host_allowed(&host) {
                next.run(request).await
            } else {
                warn!(
                    host = %host,
                    "Rejected request with invalid Host header (possible DNS rebinding attack)"
                );
                forbidden_response()
            }
        }
        None => {
            warn!("Rejected request without Host header");
            forbidden_response()
        }
    }
}

fn forbidden_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        [(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain"),
        )],
        Body::from("Forbidden: Invalid Host header"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use http::{Method, Request, StatusCode};
    use rstest::rstest;
    use tower::util::ServiceExt;

    fn test_router(config: HostValidationConfig, port: u16) -> Router {
        Router::new().route("/test", get(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(
                HostValidationState {
                    config: Arc::new(config),
                    server_port: port,
                },
                validate_host,
            ),
        )
    }

    fn default_state() -> HostValidationState {
        HostValidationState {
            config: Arc::new(HostValidationConfig::default()),
            server_port: 8000,
        }
    }

    mod is_host_allowed {
        use super::*;

        #[rstest]
        #[case::localhost("localhost", true)]
        #[case::localhost_with_port("localhost:8000", true)]
        #[case::ipv4_loopback("127.0.0.1:8000", true)]
        #[case::ipv6_loopback("[::1]:8000", true)]
        #[case::unspecified_ipv4("0.0.0.0:8000", true)]
        #[case::wrong_port("localhost:9999", false)]
        #[case::attacker_without_port("attacker.com", false)]
        #[case::attacker_with_port("attacker.com:8000", false)]
        fn default_config(#[case] host: &str, #[case] expected: bool) {
            assert_eq!(default_state().is_host_allowed(host), expected);
        }

        #[test]
        fn disabled_allows_any_host() {
            let state = HostValidationState {
                config: Arc::new(HostValidationConfig::disabled()),
                server_port: 8000,
            };
            assert!(state.is_host_allowed("attacker.com"));
        }
    }

    mod validate_host_middleware {
        use super::*;

        #[rstest]
        #[case::localhost("localhost:8000", StatusCode::OK)]
        #[case::localhost_without_port("localhost", StatusCode::OK)]
        #[case::ipv4_loopback("127.0.0.1:8000", StatusCode::OK)]
        #[case::ipv6_loopback("[::1]:8000", StatusCode::OK)]
        #[case::unspecified_ipv4("0.0.0.0:8000", StatusCode::OK)]
        #[case::case_insensitive("LOCALHOST:8000", StatusCode::OK)]
        #[case::attacker("attacker.com", StatusCode::FORBIDDEN)]
        #[case::attacker_with_port("attacker.com:8000", StatusCode::FORBIDDEN)]
        #[case::wrong_port("localhost:9999", StatusCode::FORBIDDEN)]
        #[tokio::test]
        async fn default_config(#[case] host: &str, #[case] expected: StatusCode) {
            let app = test_router(HostValidationConfig::default(), 8000);
            let request = Request::builder()
                .method(Method::GET)
                .uri("/test")
                .header("Host", host)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected);
        }

        #[tokio::test]
        async fn disabled_allows_any_host() {
            let app = test_router(HostValidationConfig::disabled(), 8000);
            let request = Request::builder()
                .method(Method::GET)
                .uri("/test")
                .header("Host", "attacker.com")
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[rstest]
        #[case::matching_host("mcp.test.com", StatusCode::OK)]
        #[case::wrong_host("attacker.com", StatusCode::FORBIDDEN)]
        #[tokio::test]
        async fn custom_allowed_host(#[case] host: &str, #[case] expected: StatusCode) {
            let config = HostValidationConfig {
                enabled: true,
                allowed_hosts: vec!["mcp.test.com".to_string()],
            };
            let app = test_router(config, 8000);
            let request = Request::builder()
                .method(Method::GET)
                .uri("/test")
                .header("Host", host)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected);
        }

        #[rstest]
        #[case::matching_port("mcp.test.com:8000", StatusCode::OK)]
        #[case::wrong_port("mcp.test.com:9000", StatusCode::FORBIDDEN)]
        #[tokio::test]
        async fn custom_allowed_host_with_port(#[case] host: &str, #[case] expected: StatusCode) {
            let config = HostValidationConfig {
                enabled: true,
                allowed_hosts: vec!["mcp.test.com:8000".to_string()],
            };
            let app = test_router(config, 8000);
            let request = Request::builder()
                .method(Method::GET)
                .uri("/test")
                .header("Host", host)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[test]
    fn default_config_is_enabled() {
        let config = HostValidationConfig::default();
        assert!(config.enabled);
    }

    #[test]
    fn default_config_has_no_allowed_hosts() {
        let config = HostValidationConfig::default();
        assert!(config.allowed_hosts.is_empty());
    }
}
