//! Contract tests for auth's SEP-2243 fast path. Auth decides from the
//! `Mcp-Method` / `Mcp-Name` headers without reading the body, which is safe
//! only because rmcp rejects headers that disagree with the body or repeat.
//! rmcp tests that behavior itself; these pin it for this server's wiring, so an
//! rmcp upgrade or transport change cannot quietly turn it into an auth bypass.

use axum::{Router, body::Body};
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use rstest::rstest;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    test_support::{SseReader, create_test_running, next_message},
    *,
};

fn router(handler: McpService, stateful: bool) -> Router {
    let cancel = handler.application.cancellation_token.clone();
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(stateful)
            .with_json_response(true)
            .with_cancellation_token(cancel),
    );
    let auth: crate::auth::Config = serde_yaml::from_str(
        r#"
        servers: [https://auth.example.com]
        resource: http://localhost/mcp
        skip_token_validation:
          methods:
            - server/discover
            - tools/list
            - resources/list
            - initialize
            - notifications/initialized
          tools:
            - Public
        "#,
    )
    .unwrap();
    auth.enable_middleware(
        Router::new().nest_service("/mcp", service),
        HashMap::new(),
        stateful,
    )
    .unwrap()
}

fn request(method: &str, version: &str, header: Option<&str>) -> Request<Body> {
    let mut params = if method == "initialize" {
        json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "header-test", "version": "1"}})
    } else {
        json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "header-test", "version": "1"}
        }})
    };
    if method == "tools/call" {
        params["name"] = json!("Protected");
    }
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", version);
    if let Some(header) = header {
        builder = builder.header("Mcp-Method", header);
    }
    builder
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string(),
        ))
        .unwrap()
}

async fn json_response(response: axum::response::Response) -> Value {
    if response.headers().get("Content-Type").unwrap() == "text/event-stream" {
        next_message(&mut SseReader::new(response.into_body())).await
    } else {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
}

#[rstest]
#[case::stateless(false)]
#[case::stateful(true)]
#[tokio::test]
async fn header_admitted_request_cannot_run_a_different_method(#[case] stateful: bool) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // Auth admits the skip-listed `tools/list` header; the body is a tools/call.
    let response = router(running.for_service(), stateful)
        .oneshot(request(
            "tools/call",
            ProtocolVersion::STANDARD_HEADERS.as_str(),
            Some("tools/list"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_response(response).await;
    assert_eq!(body["error"]["code"], ErrorCode::HEADER_MISMATCH.0);
}

#[rstest]
#[case::stateless(false)]
#[case::stateful(true)]
#[tokio::test]
async fn header_admitted_request_cannot_call_a_different_tool(#[case] stateful: bool) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // Auth admits the skip-listed `Public` name; the body calls `Protected`.
    // `required_scopes` reads the same name, so this also guards scope checks.
    let mut req = request(
        "tools/call",
        ProtocolVersion::STANDARD_HEADERS.as_str(),
        Some("tools/call"),
    );
    req.headers_mut()
        .insert("Mcp-Name", http::HeaderValue::from_static("Public"));
    let response = router(running.for_service(), stateful)
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_response(response).await;
    assert_eq!(body["error"]["code"], ErrorCode::HEADER_MISMATCH.0);
}

#[rstest]
#[case::stateless(false)]
#[case::stateful(true)]
#[tokio::test]
async fn header_admitted_initialize_cannot_start_the_lifecycle(#[case] stateful: bool) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // Retain the same handler the transport serves, so a failed handshake cannot
    // hide application initialization by dropping its notification receiver.
    let handler = running.for_service();
    let response = router(handler.clone(), stateful)
        .oneshot(request(
            "initialize",
            ProtocolVersion::STANDARD_HEADERS.as_str(),
            Some("tools/list"),
        ))
        .await
        .unwrap();
    assert!(response.headers().get("Mcp-Session-Id").is_none());
    let body = json_response(response).await;
    assert_eq!(body["error"]["code"], ErrorCode::HEADER_MISMATCH.0);
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
}

#[tokio::test]
async fn repeated_method_header_cannot_pass_auth_and_dispatch() {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // Auth reads the first value, the skip-listed `tools/list`, while the body
    // and the second value say tools/call.
    let mut req = request(
        "tools/call",
        ProtocolVersion::STANDARD_HEADERS.as_str(),
        Some("tools/list"),
    );
    req.headers_mut()
        .append("Mcp-Method", http::HeaderValue::from_static("tools/call"));
    let response = router(running.for_service(), false)
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_response(response).await;
    assert_eq!(body["error"]["code"], ErrorCode::HEADER_MISMATCH.0);
}

#[tokio::test]
async fn repeated_name_header_cannot_pass_auth_and_dispatch() {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // Auth reads the first value, the skip-listed `Public`, while the body and
    // the second value name `Protected`.
    let mut req = request(
        "tools/call",
        ProtocolVersion::STANDARD_HEADERS.as_str(),
        Some("tools/call"),
    );
    req.headers_mut()
        .append("Mcp-Name", http::HeaderValue::from_static("Public"));
    req.headers_mut()
        .append("Mcp-Name", http::HeaderValue::from_static("Protected"));
    let response = router(running.for_service(), false)
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_response(response).await;
    assert_eq!(body["error"]["code"], ErrorCode::HEADER_MISMATCH.0);
}

#[rstest]
#[case(None)]
#[case(Some("initialize"))]
#[tokio::test]
async fn legitimate_initialize_still_starts_lifecycle(
    #[case] header: Option<&str>,
    #[values("2025-11-25", "2026-07-28")] version: &str,
) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    let handler = running.for_service();
    let response = router(handler.clone(), false)
        .oneshot(request("initialize", version, header))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(running.tool_list_changes.receiver_count(), 1);
    drop(handler);
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
}

#[rstest]
#[case(None)]
#[case(Some("server/discover"))]
#[tokio::test]
async fn discovery_on_supported_version_preserves_legacy_fallback(#[case] header: Option<&str>) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    let response = router(running.for_service(), false)
        .oneshot(request("server/discover", "2025-11-25", header))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(json_response(response).await.get("result").is_some());
}
