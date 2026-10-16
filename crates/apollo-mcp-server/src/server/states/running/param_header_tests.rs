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
use crate::apps::{
    App,
    app::{AppResource, AppResourceSource, AppTool},
    manifest::AppLabels,
};
use crate::operations::{ParamHeaders, app_tool_collisions};

const HEADER: &str = "Mcp-Param-Workflow-Handle";

async fn running_with_param_header(endpoint: url::Url) -> Running {
    running_with_param_header_on(endpoint, "GetWorkflow").await
}

async fn running_with_param_header_on(endpoint: url::Url, operation_name: &str) -> Running {
    running_with_typed_param_header(endpoint, operation_name, "ID!", "workflow").await
}

/// Annotates `$handle: {handle_type}`, passed to the `Query` field `field` that accepts it.
async fn running_with_typed_param_header(
    endpoint: url::Url,
    operation_name: &str,
    handle_type: &str,
    field: &str,
) -> Running {
    let schema = Schema::parse_and_validate(
        "type Query { workflow(handle: ID!): String count(handle: Int!): String flag(handle: Boolean!): String }",
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
        format!("query {operation_name}($handle: {handle_type}) {{ {field}(handle: $handle) }}"),
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
    running.operations = Arc::new(RwLock::new(vec![operation]));
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

fn tools_call(handle: impl Into<Value>, header: Option<&str>) -> Request<Body> {
    tools_call_on(ProtocolVersion::STANDARD_HEADERS, handle, header)
}

fn tools_call_on(
    version: ProtocolVersion,
    handle: impl Into<Value>,
    header: Option<&str>,
) -> Request<Body> {
    tools_call_at("/mcp", version, handle, header)
}

fn tools_call_at(
    uri: &str,
    version: ProtocolVersion,
    handle: impl Into<Value>,
    header: Option<&str>,
) -> Request<Body> {
    let handle = handle.into();
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
        .uri(uri)
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
    send_to(&router(running), request).await
}

async fn send_to(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
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

/// rmcp compares the header text with the body value's JSON text, so non-string
/// parameters match without the property `type` that `get_tool` leaves out.
#[rstest::rstest]
#[case::int("Int!", "count", json!(42), "42")]
#[case::boolean("Boolean!", "flag", json!(true), "true")]
#[tokio::test]
async fn non_string_param_header_matching_the_body_runs_the_operation(
    #[case] handle_type: &str,
    #[case] field: &str,
    #[case] handle: Value,
    #[case] header: &str,
) {
    let mut upstream = mockito::Server::new_async().await;
    let graphql = upstream
        .mock("POST", "/")
        .with_header("Content-Type", "application/json")
        .with_body(format!(r#"{{"data": {{"{field}": "ok"}}}}"#))
        .create_async()
        .await;
    let running = running_with_typed_param_header(
        upstream.url().parse().unwrap(),
        "GetWorkflow",
        handle_type,
        field,
    )
    .await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, body) = send(&running, tools_call(handle, Some(header))).await;

    graphql.assert_async().await;
    assert_eq!(
        (status, body["result"]["isError"].clone()),
        (StatusCode::OK, json!(false))
    );
}

#[tokio::test]
async fn mismatched_int_param_header_is_rejected() {
    let running = running_with_typed_param_header(
        "http://localhost:4000".parse().unwrap(),
        "GetWorkflow",
        "Int!",
        "count",
    )
    .await;
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, body) = send(&running, tools_call(42, Some("43"))).await;

    assert_eq!(
        (status, body["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!(ErrorCode::HEADER_MISMATCH.0))
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

/// rmcp caches `get_tool` per name for the life of the service, so a call made before the
/// operation loads must not leave the tool unchecked once a reload loads it.
#[tokio::test]
async fn operation_loaded_after_first_call_is_still_checked() {
    let running = running_with_param_header("http://localhost:4000".parse().unwrap()).await;
    let loaded = std::mem::take(&mut *running.operations.write().await);
    let _shutdown = running.cancellation_token.clone().drop_guard();
    // One router, so rmcp's per-service cache sees both calls.
    let router = router(&running);
    send_to(&router, tools_call("wf-1", Some("wf-1"))).await;

    *running.operations.write().await = loaded;
    let (status, body) = send_to(&router, tools_call("wf-1", Some("wf-2"))).await;

    assert_eq!(
        (status, body["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!(ErrorCode::HEADER_MISMATCH.0))
    );
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

/// An app whose `GetWorkflow` tool reuses the annotated operation's name.
async fn app_reusing_operation_name(running: &Running) -> App {
    let operation = running.operations.read().await[0].clone();
    App {
        name: "MyApp".to_string(),
        description: None,
        tools: vec![AppTool {
            tool: Tool::new("GetWorkflow", "an app tool", rmcp::model::JsonObject::new()),
            operation: Arc::new(operation),
            labels: AppLabels::default(),
            extra_outputs: None,
        }],
        resource: AppResource::Single(AppResourceSource::Local("content".into())),
        uri: "http://localhost:4000/resource".parse().unwrap(),
        prefetch_operations: vec![],
        csp_settings: None,
        widget_settings: None,
    }
}

#[tokio::test]
async fn app_tool_sharing_an_annotated_operation_name_is_reported() {
    let running = running_with_param_header("http://localhost:4000".parse().unwrap()).await;
    let apps = [app_reusing_operation_name(&running).await];

    assert_eq!(
        app_tool_collisions(&running.param_headers, &apps),
        vec![("MyApp", "GetWorkflow")]
    );
}

/// Pins the limitation the startup warning describes: `get_tool` can't see the `app`
/// query param, so the operation's annotation applies to the app tool call too.
#[tokio::test]
async fn app_mode_call_to_tool_sharing_an_annotated_name_is_rejected() {
    let mut running = running_with_param_header("http://localhost:4000".parse().unwrap()).await;
    running.apps = vec![app_reusing_operation_name(&running).await];
    let _shutdown = running.cancellation_token.clone().drop_guard();

    let (status, body) = send(
        &running,
        tools_call_at(
            "/mcp?app=MyApp",
            ProtocolVersion::STANDARD_HEADERS,
            "wf-1",
            None,
        ),
    )
    .await;

    assert_eq!(
        (status, body["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!(ErrorCode::HEADER_MISMATCH.0))
    );
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
