//! SEP-2243 `Mcp-Param-*` headers on predefined operations. rmcp compares each
//! header with the body using the tool that `get_tool` returns; these tests pin
//! that wiring so a request can't carry one value in the header and run another.

use axum::{Router, body::Body};
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{test_support::create_test_running, *};
use crate::operations::ParamHeaders;

const HEADER: &str = "Mcp-Param-Workflow-Handle";

async fn running_with_param_header(endpoint: url::Url) -> Running {
    running_with_param_header_on(endpoint, "GetWorkflow").await
}

async fn running_with_param_header_on(endpoint: url::Url, operation_name: &str) -> Running {
    let schema = Schema::parse_and_validate(
        "type Query { workflow(handle: ID!): String }",
        "schema.graphql",
    )
    .unwrap();
    let param_headers = HashMap::from([(
        operation_name.to_string(),
        ParamHeaders::try_from(HashMap::from([(
            "handle".to_string(),
            "Workflow-Handle".to_string(),
        )]))
        .unwrap(),
    )]);
    let operation = RawOperation::from((
        format!("query {operation_name}($handle: ID!) {{ workflow(handle: $handle) }}"),
        None,
    ))
    .into_operation(
        &schema,
        None,
        MutationMode::None,
        false,
        false,
        false,
        &HashMap::new(),
        &HashMap::new(),
        &param_headers,
    )
    .unwrap()
    .unwrap();

    let mut running = create_test_running();
    *running.schema.write().await = schema;
    running.operations = OperationCatalog::new(vec![operation]);
    running.param_headers = param_headers;
    running.endpoint = endpoint;
    running
}

fn router(running: &Running) -> Router {
    let handler = running.for_service();
    Router::new().nest_service(
        "/mcp",
        StreamableHttpService::new(
            move || Ok(handler.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(true)
                .with_cancellation_token(running.cancellation_token.clone()),
        ),
    )
}

fn tools_call(handle: &str, header: Option<&str>) -> Request<Body> {
    tools_call_on(ProtocolVersion::STANDARD_HEADERS, handle, header)
}

fn tools_call_on(version: ProtocolVersion, handle: &str, header: Option<&str>) -> Request<Body> {
    let version = version.as_str();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": version,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {"name": "param-header-test", "version": "1"}
            },
            "name": "GetWorkflow",
            "arguments": {"handle": handle}
        }
    });
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", version)
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "GetWorkflow");
    if let Some(header) = header {
        builder = builder.header(HEADER, header);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn send(running: &Running, request: Request<Body>) -> (StatusCode, Value) {
    let response = router(running).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn matching_param_header_runs_the_operation() {
    let mut upstream = mockito::Server::new_async().await;
    let graphql = upstream
        .mock("POST", "/")
        .with_header("Content-Type", "application/json")
        .with_body(r#"{"data": {"workflow": "running"}}"#)
        .create_async()
        .await;
    let running = running_with_param_header(upstream.url().parse().unwrap()).await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, body) = send(&running, tools_call("wf-1", Some("wf-1"))).await;

    graphql.assert_async().await;
    assert_eq!(
        (status, body["result"]["isError"].clone()),
        (StatusCode::OK, json!(false))
    );
}

#[tokio::test]
async fn mismatched_param_header_is_rejected() {
    let running = running_with_param_header("http://localhost:4000".parse().unwrap()).await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, body) = send(&running, tools_call("wf-1", Some("wf-2"))).await;

    assert_eq!(
        (status, body["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!(ErrorCode::HEADER_MISMATCH.0))
    );
}

#[tokio::test]
async fn missing_param_header_is_rejected() {
    let running = running_with_param_header("http://localhost:4000".parse().unwrap()).await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, body) = send(&running, tools_call("wf-1", None)).await;

    assert_eq!(
        (status, body["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!(ErrorCode::HEADER_MISMATCH.0))
    );
}

#[tokio::test]
async fn base64_param_header_is_decoded_before_comparison() {
    let mut upstream = mockito::Server::new_async().await;
    let graphql = upstream
        .mock("POST", "/")
        .with_header("Content-Type", "application/json")
        .with_body(r#"{"data": {"workflow": "running"}}"#)
        .create_async()
        .await;
    let running = running_with_param_header(upstream.url().parse().unwrap()).await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    // "  wf 1  " has leading and trailing spaces, so clients must send it Base64-encoded.
    let (status, _) = send(
        &running,
        tools_call("  wf 1  ", Some("=?base64?ICB3ZiAxICA=?=")),
    )
    .await;

    graphql.assert_async().await;
    assert_eq!(status, StatusCode::OK);
}

/// The check is a SEP-2243 rule, so a request that declares an earlier protocol skips it.
/// The docs tell proxies to require `2026-07-28` before trusting the header.
#[tokio::test]
async fn earlier_protocol_skips_the_param_header_check() {
    let mut upstream = mockito::Server::new_async().await;
    let graphql = upstream
        .mock("POST", "/")
        .with_header("Content-Type", "application/json")
        .with_body(r#"{"data": {"workflow": "running"}}"#)
        .create_async()
        .await;
    let running = running_with_param_header(upstream.url().parse().unwrap()).await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, _) = send(
        &running,
        tools_call_on(ProtocolVersion::V_2025_11_25, "wf-1", Some("wf-2")),
    )
    .await;

    graphql.assert_async().await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn builtin_tool_name_takes_precedence_over_operation_annotation() {
    let running =
        running_with_param_header_on("http://localhost:4000".parse().unwrap(), EXECUTE_TOOL_NAME)
            .await;
    let running = Running {
        execute_tool: Some(Execute::new(MutationMode::None, None)),
        ..running
    };

    assert!(running.for_service().get_tool(EXECUTE_TOOL_NAME).is_none());
}

#[tokio::test]
async fn tools_list_annotates_the_param() {
    let running = running_with_param_header("http://localhost:4000".parse().unwrap()).await;

    let tools = running
        .list_tools_impl(Extensions::default(), PeerContext::default())
        .await
        .unwrap()
        .tools;

    assert_eq!(
        tools[0].input_schema["properties"]["handle"],
        json!({"type": "string", "x-mcp-header": "Workflow-Handle"})
    );
}
