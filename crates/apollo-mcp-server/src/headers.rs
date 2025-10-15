use std::ops::Deref;
use std::str::FromStr;

use headers::HeaderMapExt;
use http::Extensions;
use reqwest::header::{HeaderMap, HeaderName};

use crate::auth::ValidToken;

/// List of header names to forward from MCP clients to GraphQL API
pub type ForwardHeaders = Vec<String>;

/// Build headers for a GraphQL request by combining static headers with forwarded headers
pub fn build_request_headers(
    static_headers: &HeaderMap,
    forward_header_names: &ForwardHeaders,
    incoming_headers: &HeaderMap,
    extensions: &Extensions,
    disable_auth_token_passthrough: bool,
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

    headers
}

/// Forward matching headers from incoming headers to outgoing headers
fn forward_headers(names: &[String], incoming: &HeaderMap, outgoing: &mut HeaderMap) {
    for header in names {
        if let Ok(header_name) = HeaderName::from_str(header)
            && let Some(value) = incoming.get(&header_name)
            // Hop-by-hop headers are blocked per RFC 7230: https://datatracker.ietf.org/doc/html/rfc7230#section-6.1
            && !matches!(
                header_name.as_str().to_lowercase().as_str(),
                "connection"
                    | "keep-alive"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "te"
                    | "trailers"
                    | "transfer-encoding"
                    | "upgrade"
                    | "content-length"
            )
        {
            outgoing.insert(header_name, value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use headers::Authorization;
    use http::Extensions;
    use reqwest::header::HeaderValue;

    use crate::auth::ValidToken;

    #[test]
    fn test_build_request_headers_includes_static_headers() {
        let mut static_headers = HeaderMap::new();
        static_headers.insert("x-api-key", HeaderValue::from_static("static-key"));
        static_headers.insert("user-agent", HeaderValue::from_static("mcp-server"));

        let forward_header_names = vec![];
        let incoming_headers = HeaderMap::new();
        let extensions = Extensions::new();

        let result = build_request_headers(
            &static_headers,
            &forward_header_names,
            &incoming_headers,
            &extensions,
            false,
        );

        assert_eq!(result.get("x-api-key").unwrap(), "static-key");
        assert_eq!(result.get("user-agent").unwrap(), "mcp-server");
    }

    #[test]
    fn test_build_request_headers_forwards_configured_headers() {
        let static_headers = HeaderMap::new();
        let forward_header_names = vec!["x-tenant-id".to_string(), "x-trace-id".to_string()];

        let mut incoming_headers = HeaderMap::new();
        incoming_headers.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));
        incoming_headers.insert("x-trace-id", HeaderValue::from_static("trace-456"));
        incoming_headers.insert("other-header", HeaderValue::from_static("ignored"));

        let extensions = Extensions::new();

        let result = build_request_headers(
            &static_headers,
            &forward_header_names,
            &incoming_headers,
            &extensions,
            false,
        );

        assert_eq!(result.get("x-tenant-id").unwrap(), "tenant-123");
        assert_eq!(result.get("x-trace-id").unwrap(), "trace-456");
        assert!(result.get("other-header").is_none());
    }

    #[test]
    fn test_build_request_headers_adds_oauth_token_when_enabled() {
        let static_headers = HeaderMap::new();
        let forward_header_names = vec![];
        let incoming_headers = HeaderMap::new();

        let mut extensions = Extensions::new();
        let token = ValidToken(Authorization::bearer("test-token").unwrap());
        extensions.insert(token);

        let result = build_request_headers(
            &static_headers,
            &forward_header_names,
            &incoming_headers,
            &extensions,
            false,
        );

        assert!(result.get("authorization").is_some());
        assert_eq!(result.get("authorization").unwrap(), "Bearer test-token");
    }

    #[test]
    fn test_build_request_headers_skips_oauth_token_when_disabled() {
        let static_headers = HeaderMap::new();
        let forward_header_names = vec![];
        let incoming_headers = HeaderMap::new();

        let mut extensions = Extensions::new();
        let token = ValidToken(Authorization::bearer("test-token").unwrap());
        extensions.insert(token);

        let result = build_request_headers(
            &static_headers,
            &forward_header_names,
            &incoming_headers,
            &extensions,
            true,
        );

        assert!(result.get("authorization").is_none());
    }

    #[test]
    fn test_build_request_headers_forwards_mcp_session_id() {
        let static_headers = HeaderMap::new();
        let forward_header_names = vec![];

        let mut incoming_headers = HeaderMap::new();
        incoming_headers.insert("mcp-session-id", HeaderValue::from_static("session-123"));

        let extensions = Extensions::new();

        let result = build_request_headers(
            &static_headers,
            &forward_header_names,
            &incoming_headers,
            &extensions,
            false,
        );

        assert_eq!(result.get("mcp-session-id").unwrap(), "session-123");
    }

    #[test]
    fn test_build_request_headers_combined_scenario() {
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
        let token = ValidToken(Authorization::bearer("oauth-token").unwrap());
        extensions.insert(token);

        let result = build_request_headers(
            &static_headers,
            &forward_header_names,
            &incoming_headers,
            &extensions,
            false,
        );

        // Verify all parts combined correctly
        assert_eq!(result.get("x-api-key").unwrap(), "static-key");
        assert_eq!(result.get("x-tenant-id").unwrap(), "tenant-123");
        assert_eq!(result.get("mcp-session-id").unwrap(), "session-456");
        assert_eq!(result.get("authorization").unwrap(), "Bearer oauth-token");
        assert!(result.get("ignored-header").is_none());
    }

    #[test]
    fn test_forward_headers_no_headers_by_default() {
        let names: Vec<String> = vec![];

        let mut incoming = HeaderMap::new();
        incoming.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));

        let mut outgoing = HeaderMap::new();

        forward_headers(&names, &incoming, &mut outgoing);

        assert!(outgoing.is_empty());
    }

    #[test]
    fn test_forward_headers_only_specific_headers() {
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

        forward_headers(&names, &incoming, &mut outgoing);

        assert_eq!(outgoing.get("x-tenant-id").unwrap(), "tenant-123");
        assert_eq!(outgoing.get("x-trace-id").unwrap(), "trace-456");
        assert_eq!(outgoing.get("x-geo-country").unwrap(), "US");
        assert_eq!(outgoing.get("x-experiment-id").unwrap(), "exp-789");
        assert_eq!(outgoing.get("ai-client-name").unwrap(), "claude");

        assert!(outgoing.get("other-header").is_none());
    }

    #[test]
    fn test_forward_headers_blocks_hop_by_hop_headers() {
        let names = vec!["connection".to_string(), "content-length".to_string()];

        let mut incoming = HeaderMap::new();
        incoming.insert("connection", HeaderValue::from_static("keep-alive"));
        incoming.insert("content-length", HeaderValue::from_static("1234"));

        let mut outgoing = HeaderMap::new();

        forward_headers(&names, &incoming, &mut outgoing);

        assert!(outgoing.get("connection").is_none());
        assert!(outgoing.get("content-length").is_none());
    }

    #[test]
    fn test_forward_headers_case_insensitive_matching() {
        let names = vec!["X-Tenant-ID".to_string()];

        let mut incoming = HeaderMap::new();
        incoming.insert("x-tenant-id", HeaderValue::from_static("tenant-123"));

        let mut outgoing = HeaderMap::new();
        forward_headers(&names, &incoming, &mut outgoing);

        assert_eq!(outgoing.get("x-tenant-id").unwrap(), "tenant-123");
    }
}
