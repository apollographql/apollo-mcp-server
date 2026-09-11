use axum::{Router, body::Body};
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::{SessionManager, local::LocalSessionManager},
};
use rstest::rstest;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    test_support::{SseReader, create_test_running, next_message},
    *,
};

fn router(handler: McpService, stateful: bool) -> (Router, Arc<LocalSessionManager>) {
    let sessions = Arc::new(LocalSessionManager::default());
    let cancel = handler.application.cancellation_token.clone();
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        sessions.clone(),
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
        "#,
    )
    .unwrap();
    let router = auth
        .enable_middleware(
            Router::new().nest_service("/mcp", service),
            HashMap::new(),
            stateful,
        )
        .unwrap();
    (router, sessions)
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
#[case(false)]
#[case(true)]
#[tokio::test]
async fn forged_discovery_header_cannot_initialize(
    #[case] stateful: bool,
    #[values("2025-11-25", "2026-07-28")] version: &str,
) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // Retain the same handler the transport serves, so a failed handshake cannot
    // hide application initialization by dropping its notification receiver.
    let handler = running.for_service();
    let (app, sessions) = router(handler.clone(), stateful);
    let response = app
        .oneshot(request("initialize", version, Some("tools/list")))
        .await
        .unwrap();
    let session_id = response.headers().get("Mcp-Session-Id").cloned();
    let body = json_response(response).await;
    assert_eq!(body["error"]["code"], ErrorCode::HEADER_MISMATCH.0);
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
    if stateful {
        // rmcp allocates a legacy session before calling initialize and may
        // return its ID with the JSON-RPC error. Its worker must close it.
        let id = session_id.unwrap().to_str().unwrap().to_owned().into();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while sessions.has_session(&id).await.unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed initialization must release its session");
    } else {
        assert!(session_id.is_none());
    }
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
        .0
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
#[case(false)]
#[case(true)]
#[tokio::test]
async fn initialize_guard_rejects_malformed_and_duplicate_headers(#[case] duplicate: bool) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    let handler = running.for_service();
    // The old version deliberately takes auth's body fallback, exercising the
    // handler guard independently of the fast path's header validation.
    let mut req = request("initialize", "2025-11-25", Some("initialize"));
    if duplicate {
        req.headers_mut()
            .append("Mcp-Method", http::HeaderValue::from_static("initialize"));
    } else {
        req.headers_mut().insert(
            "Mcp-Method",
            http::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
    }
    let response = router(handler.clone(), false).0.oneshot(req).await.unwrap();
    assert_eq!(
        json_response(response).await["error"]["code"],
        ErrorCode::HEADER_MISMATCH.0
    );
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
}

#[rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn sdk_rejects_forged_tool_call_after_header_bypass(#[case] stateful: bool) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    let response = router(running.for_service(), stateful)
        .0
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
#[case(None)]
#[case(Some("server/discover"))]
#[tokio::test]
async fn discovery_on_supported_version_preserves_legacy_fallback(#[case] header: Option<&str>) {
    let running = create_test_running();
    let _shutdown = running.cancellation_token.clone().drop_guard();
    let response = router(running.for_service(), false)
        .0
        .oneshot(request("server/discover", "2025-11-25", header))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(json_response(response).await.get("result").is_some());
}
