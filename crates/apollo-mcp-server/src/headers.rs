use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;

use headers::HeaderMapExt;
use http::Extensions;
use reqwest::header::{HeaderMap, HeaderName};
use tracing::warn;

use crate::auth::ValidToken;

/// List of header names to forward from MCP clients to GraphQL API
pub type ForwardHeaders = Vec<String>;

/// A callback that transforms HTTP headers before they are sent to the upstream GraphQL endpoint.
///
/// The callback receives a mutable reference to the fully assembled headers (after static headers,
/// forwarded headers, auth token passthrough, and mcp-session-id have all been applied).
/// This enables custom authentication schemes, header-based routing, HMAC signing, and other
/// transformations without requiring an intermediary proxy.
pub type HeaderTransform = Arc<dyn Fn(&mut HeaderMap) + Send + Sync>;

/// Build headers for a GraphQL request by combining static headers with forwarded headers
pub fn build_request_headers(
    static_headers: &HeaderMap,
    forward_header_names: &ForwardHeaders,
    incoming_headers: &HeaderMap,
    extensions: &Extensions,
    disable_auth_token_passthrough: bool,
    header_transform: Option<&HeaderTransform>,
) -> HeaderMap {
    // Starts with static headers
    let mut headers = static_headers.clone();

    // Forward headers dynamically
    forward_headers(forward_header_names, incoming_headers, &mut headers);

    // Optionally extract the validated token and propagate it to upstream servers if present
    if !disable_auth_token_passthrough && let Some(token) = extensions.get::<ValidToken>() {
        headers.typed_insert(token.deref().clone());
    }

    // Forward the mcp-session-id header if present
    if let Some(session_id) = incoming_headers.get("mcp-session-id") {
        headers.insert("mcp-session-id", session_id.clone());
    }

    // Apply consumer-provided header transformation
    if let Some(transform) = header_transform {
        transform(&mut headers);
    }

    headers
}

/// Forward matching headers from incoming headers to outgoing headers
fn forward_headers(names: &[String], incoming: &HeaderMap, outgoing: &mut HeaderMap) {
    for header in names {
        if let Ok(header_name) = HeaderName::from_str(header)
            && let Some(value) = incoming.get(&header_name)
        {
            if matches!(
                header_name.as_str(),
                "authorization" | "cookie" | "proxy-authorization" | "x-api-key"
            ) {
                warn!(
                    header = %header_name,
                    "Forwarding sensitive header to upstream GraphQL API"
                );
            }
            // Hop-by-hop headers are blocked per RFC 7230: https://datatracker.ietf.org/doc/html/rfc7230#section-6.1
            if matches!(
                header_name.as_str(),
                "connection"
                    | "keep-alive"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "te"
                    | "trailers"
                    | "transfer-encoding"
                    | "upgrade"
                    | "content-length"
            ) {
                continue;
            }
            outgoing.insert(header_name, value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    mod build_request_headers {
        use super::*;
        use headers::Authorization;
        use http::Extensions;

        use crate::auth::ValidToken;

        #[test]
        fn includes_static_headers() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("static-key"));
            static_headers.insert("user-agent", HeaderValue::from_static("mcp-server"));

            let forward_header_names = vec![];
            let incoming_headers = HeaderMap::new();
            let extensions = Extensions::new();

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                None,
            );

            assert_eq!(result.get("x-api-key").unwrap(), "static-key");
            assert_eq!(result.get("user-agent").unwrap(), "mcp-server");
        }

        #[test]
        fn forwards_configured_headers() {
            let static_headers = HeaderMap::new();
            let forward_header_names = vec!["x-tenant-id".to_string(), "x-trace-id".to_string()];

            let mut incoming_headers = HeaderMap::new();
            incoming_headers.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));
            incoming_headers.insert("x-trace-id", HeaderValue::from_static("trace-456"));
            incoming_headers.insert("other-header", HeaderValue::from_static("ignored"));

            let extensions = Extensions::new();

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                None,
            );

            assert_eq!(result.get("x-tenant-id").unwrap(), "tenant-123");
            assert_eq!(result.get("x-trace-id").unwrap(), "trace-456");
            assert!(result.get("other-header").is_none());
        }

        #[test]
        fn adds_oauth_token_when_enabled() {
            let static_headers = HeaderMap::new();
            let forward_header_names = vec![];
            let incoming_headers = HeaderMap::new();

            let mut extensions = Extensions::new();
            let token = ValidToken {
                token: Authorization::bearer("test-token").unwrap(),
                scopes: vec![],
            };
            extensions.insert(token);

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                None,
            );

            assert!(result.get("authorization").is_some());
            assert_eq!(result.get("authorization").unwrap(), "Bearer test-token");
        }

        #[test]
        fn skips_oauth_token_when_disabled() {
            let static_headers = HeaderMap::new();
            let forward_header_names = vec![];
            let incoming_headers = HeaderMap::new();

            let mut extensions = Extensions::new();
            let token = ValidToken {
                token: Authorization::bearer("test-token").unwrap(),
                scopes: vec![],
            };
            extensions.insert(token);

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                true,
                None,
            );

            assert!(result.get("authorization").is_none());
        }

        #[test]
        fn forwards_mcp_session_id() {
            let static_headers = HeaderMap::new();
            let forward_header_names = vec![];

            let mut incoming_headers = HeaderMap::new();
            incoming_headers.insert("mcp-session-id", HeaderValue::from_static("session-123"));

            let extensions = Extensions::new();

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                None,
            );

            assert_eq!(result.get("mcp-session-id").unwrap(), "session-123");
        }

        #[test]
        fn combined_scenario() {
            // Static headers
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("static-key"));

            // Forward specific headers
            let forward_header_names = vec!["x-tenant-id".to_string()];

            // Incoming headers
            let mut incoming_headers = HeaderMap::new();
            incoming_headers.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));
            incoming_headers.insert("mcp-session-id", HeaderValue::from_static("session-456"));
            incoming_headers.insert(
                "ignored-header",
                HeaderValue::from_static("should-not-appear"),
            );

            // OAuth token
            let mut extensions = Extensions::new();
            let token = ValidToken {
                token: Authorization::bearer("oauth-token").unwrap(),
                scopes: vec![],
            };
            extensions.insert(token);

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                None,
            );

            // Verify all parts combined correctly
            assert_eq!(result.get("x-api-key").unwrap(), "static-key");
            assert_eq!(result.get("x-tenant-id").unwrap(), "tenant-123");
            assert_eq!(result.get("mcp-session-id").unwrap(), "session-456");
            assert_eq!(result.get("authorization").unwrap(), "Bearer oauth-token");
            assert!(result.get("ignored-header").is_none());
        }
    }

    mod header_transform {
        use super::*;

        #[test]
        fn applies_transform_to_headers() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("static-key"));

            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(|headers: &mut HeaderMap| {
                    headers.insert("x-injected", HeaderValue::from_static("injected-value"));
                });

            let result = super::super::build_request_headers(
                &static_headers,
                &vec![],
                &HeaderMap::new(),
                &Extensions::new(),
                false,
                Some(&transform),
            );

            assert_eq!(result.get("x-api-key").unwrap(), "static-key");
            assert_eq!(result.get("x-injected").unwrap(), "injected-value");
        }

        #[test]
        fn transform_can_modify_existing_headers() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("authorization", HeaderValue::from_static("original-auth"));

            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(|headers: &mut HeaderMap| {
                    headers.insert(
                        "authorization",
                        HeaderValue::from_static("transformed-auth"),
                    );
                });

            let result = super::super::build_request_headers(
                &static_headers,
                &vec![],
                &HeaderMap::new(),
                &Extensions::new(),
                false,
                Some(&transform),
            );

            assert_eq!(result.get("authorization").unwrap(), "transformed-auth");
        }

        #[test]
        fn transform_runs_after_all_other_headers() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-static", HeaderValue::from_static("static-value"));

            let forward_header_names = vec!["x-forwarded".to_string()];

            let mut incoming_headers = HeaderMap::new();
            incoming_headers.insert("x-forwarded", HeaderValue::from_static("forwarded-value"));
            incoming_headers.insert("mcp-session-id", HeaderValue::from_static("session-abc"));

            let extensions = Extensions::new();

            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(|headers: &mut HeaderMap| {
                    // Verify all headers are present before transform runs
                    assert!(headers.contains_key("x-static"));
                    assert!(headers.contains_key("x-forwarded"));
                    assert!(headers.contains_key("mcp-session-id"));
                    headers.insert("x-transform-ran", HeaderValue::from_static("true"));
                });

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                Some(&transform),
            );

            assert_eq!(result.get("x-static").unwrap(), "static-value");
            assert_eq!(result.get("x-forwarded").unwrap(), "forwarded-value");
            assert_eq!(result.get("mcp-session-id").unwrap(), "session-abc");
            assert_eq!(result.get("x-transform-ran").unwrap(), "true");
        }

        #[test]
        fn none_transform_is_noop() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("key"));

            let result = super::super::build_request_headers(
                &static_headers,
                &vec![],
                &HeaderMap::new(),
                &Extensions::new(),
                false,
                None,
            );

            assert_eq!(result.get("x-api-key").unwrap(), "key");
        }

        #[test]
        fn transform_can_remove_headers() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("static-key"));
            static_headers.insert("x-secret", HeaderValue::from_static("should-be-removed"));

            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(|headers: &mut HeaderMap| {
                    headers.remove("x-secret");
                });

            let result = super::super::build_request_headers(
                &static_headers,
                &vec![],
                &HeaderMap::new(),
                &Extensions::new(),
                false,
                Some(&transform),
            );

            assert_eq!(result.get("x-api-key").unwrap(), "static-key");
            assert!(result.get("x-secret").is_none());
        }

        #[test]
        fn transform_can_override_auth_token() {
            use headers::Authorization;

            let static_headers = HeaderMap::new();
            let forward_header_names = vec![];
            let incoming_headers = HeaderMap::new();

            let mut extensions = Extensions::new();
            let token = ValidToken {
                token: Authorization::bearer("original-token").unwrap(),
                scopes: vec![],
            };
            extensions.insert(token);

            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(|headers: &mut HeaderMap| {
                    headers.insert(
                        "authorization",
                        HeaderValue::from_static("Bearer custom-signed-token"),
                    );
                });

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                Some(&transform),
            );

            // The transform should override the auth token passthrough
            assert_eq!(
                result.get("authorization").unwrap(),
                "Bearer custom-signed-token"
            );
        }

        #[test]
        fn transform_is_arc_cloneable_and_reusable() {
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("key"));

            let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let call_count_clone = call_count.clone();

            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(move |headers: &mut HeaderMap| {
                    call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    headers.insert("x-request-id", HeaderValue::from_static("unique"));
                });

            // Clone the Arc to simulate sharing across threads/invocations
            let transform_clone = transform.clone();

            let result1 = super::super::build_request_headers(
                &static_headers,
                &vec![],
                &HeaderMap::new(),
                &Extensions::new(),
                false,
                Some(&transform),
            );

            let result2 = super::super::build_request_headers(
                &static_headers,
                &vec![],
                &HeaderMap::new(),
                &Extensions::new(),
                false,
                Some(&transform_clone),
            );

            assert_eq!(result1.get("x-request-id").unwrap(), "unique");
            assert_eq!(result2.get("x-request-id").unwrap(), "unique");
            assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        }

        #[test]
        fn combined_scenario_with_transform() {
            use headers::Authorization;

            // Static headers
            let mut static_headers = HeaderMap::new();
            static_headers.insert("x-api-key", HeaderValue::from_static("static-key"));

            // Forward specific headers
            let forward_header_names = vec!["x-tenant-id".to_string()];

            // Incoming headers
            let mut incoming_headers = HeaderMap::new();
            incoming_headers.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));
            incoming_headers.insert("mcp-session-id", HeaderValue::from_static("session-456"));

            // OAuth token
            let mut extensions = Extensions::new();
            let token = ValidToken {
                token: Authorization::bearer("oauth-token").unwrap(),
                scopes: vec![],
            };
            extensions.insert(token);

            // Transform that adds HMAC signature based on all other headers
            let transform: super::super::HeaderTransform =
                std::sync::Arc::new(|headers: &mut HeaderMap| {
                    // Simulate HMAC signing: the transform can read existing headers
                    let has_api_key = headers.contains_key("x-api-key");
                    let has_tenant = headers.contains_key("x-tenant-id");
                    let has_session = headers.contains_key("mcp-session-id");
                    let has_auth = headers.contains_key("authorization");
                    assert!(has_api_key && has_tenant && has_session && has_auth);
                    headers.insert("x-hmac-signature", HeaderValue::from_static("sig-abc123"));
                });

            let result = super::super::build_request_headers(
                &static_headers,
                &forward_header_names,
                &incoming_headers,
                &extensions,
                false,
                Some(&transform),
            );

            // All original headers are present
            assert_eq!(result.get("x-api-key").unwrap(), "static-key");
            assert_eq!(result.get("x-tenant-id").unwrap(), "tenant-123");
            assert_eq!(result.get("mcp-session-id").unwrap(), "session-456");
            assert_eq!(result.get("authorization").unwrap(), "Bearer oauth-token");
            // Plus the transform-injected header
            assert_eq!(result.get("x-hmac-signature").unwrap(), "sig-abc123");
        }
    }

    mod forward_headers {
        use super::*;
        use tracing_test::traced_test;

        #[test]
        fn no_headers_by_default() {
            let names: Vec<String> = vec![];

            let mut incoming = HeaderMap::new();
            incoming.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));

            let mut outgoing = HeaderMap::new();

            super::super::forward_headers(&names, &incoming, &mut outgoing);

            assert!(outgoing.is_empty());
        }

        #[test]
        fn only_specific_headers() {
            let names = vec![
                "x-tenant-id".to_string(),     // Multi-tenancy
                "x-trace-id".to_string(),      // Distributed tracing
                "x-geo-country".to_string(),   // Geo information from CDN
                "x-experiment-id".to_string(), // A/B testing
                "ai-client-name".to_string(),  // Client identification
            ];

            let mut incoming = HeaderMap::new();
            incoming.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));
            incoming.insert("x-trace-id", HeaderValue::from_static("trace-456"));
            incoming.insert("x-geo-country", HeaderValue::from_static("US"));
            incoming.insert("x-experiment-id", HeaderValue::from_static("exp-789"));
            incoming.insert("ai-client-name", HeaderValue::from_static("claude"));
            incoming.insert("other-header", HeaderValue::from_static("ignored"));

            let mut outgoing = HeaderMap::new();

            super::super::forward_headers(&names, &incoming, &mut outgoing);

            assert_eq!(outgoing.get("x-tenant-id").unwrap(), "tenant-123");
            assert_eq!(outgoing.get("x-trace-id").unwrap(), "trace-456");
            assert_eq!(outgoing.get("x-geo-country").unwrap(), "US");
            assert_eq!(outgoing.get("x-experiment-id").unwrap(), "exp-789");
            assert_eq!(outgoing.get("ai-client-name").unwrap(), "claude");

            assert!(outgoing.get("other-header").is_none());
        }

        #[test]
        fn blocks_hop_by_hop_headers() {
            let names = vec!["connection".to_string(), "content-length".to_string()];

            let mut incoming = HeaderMap::new();
            incoming.insert("connection", HeaderValue::from_static("keep-alive"));
            incoming.insert("content-length", HeaderValue::from_static("1234"));

            let mut outgoing = HeaderMap::new();

            super::super::forward_headers(&names, &incoming, &mut outgoing);

            assert!(outgoing.get("connection").is_none());
            assert!(outgoing.get("content-length").is_none());
        }

        #[test]
        fn case_insensitive_matching() {
            let names = vec!["X-Tenant-ID".to_string()];

            let mut incoming = HeaderMap::new();
            incoming.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));

            let mut outgoing = HeaderMap::new();
            super::super::forward_headers(&names, &incoming, &mut outgoing);

            assert_eq!(outgoing.get("x-tenant-id").unwrap(), "tenant-123");
        }

        #[test]
        #[traced_test]
        fn warns_on_sensitive_headers() {
            let names = vec![
                "authorization".to_string(),
                "cookie".to_string(),
                "proxy-authorization".to_string(),
                "x-tenant-id".to_string(),
            ];

            let mut incoming = HeaderMap::new();
            incoming.insert("authorization", HeaderValue::from_static("Bearer token"));
            incoming.insert("cookie", HeaderValue::from_static("session=abc"));
            incoming.insert(
                "proxy-authorization",
                HeaderValue::from_static("Basic creds"),
            );
            incoming.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));

            let mut outgoing = HeaderMap::new();
            super::super::forward_headers(&names, &incoming, &mut outgoing);

            assert!(logs_contain("Forwarding sensitive header"));
            assert!(logs_contain("authorization"));
            assert!(logs_contain("cookie"));
            // proxy-authorization is a hop-by-hop header so it's blocked from forwarding,
            // but we still warn that it was configured as a forwarded header
            assert!(logs_contain("proxy-authorization"));
            assert!(!logs_contain("x-tenant-id"));

            // proxy-authorization should be blocked from the outgoing headers
            assert!(outgoing.get("proxy-authorization").is_none());
        }
    }
}
