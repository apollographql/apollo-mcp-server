//! Task behavior through the production protocol handler and transport lifecycle.
use axum::{
    Json, Router,
    body::{Body, Bytes},
    routing::post,
};
use http::Request;
use http_body_util::BodyExt as _;
use rmcp::{
    ServiceExt as _,
    model::*,
    transport::{StreamableHttpServerConfig, StreamableHttpService},
};
use serde_json::json;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::task::AbortOnDropHandle;
use tower::ServiceExt as _;

use super::{test_support::create_test_running_with_operation, *};
use crate::auth::{Principal, ValidToken};
use crate::server::states::tasks::TaskRuntime;
use crate::tasks::TasksConfig;

struct Upstream {
    endpoint: Url,
    requests: mpsc::UnboundedReceiver<(HeaderMap, Value)>,
    release: Arc<Semaphore>,
    body_dropped: CancellationToken,
    _server: AbortOnDropHandle<()>,
}

impl Upstream {
    async fn new(result: Value) -> Self {
        let (sender, requests) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let body_dropped = CancellationToken::new();
        let gate = release.clone();
        let dropped = body_dropped.clone();
        let app = Router::new().route(
            "/",
            post(move |headers: HeaderMap, Json(request): Json<Value>| {
                let sender = sender.clone();
                let gate = gate.clone();
                let guard = dropped.clone().drop_guard();
                let result = result.clone();
                async move {
                    sender.send((headers, request)).unwrap();
                    Body::from_stream(futures::stream::once(async move {
                        let _guard = guard;
                        gate.acquire().await.unwrap().forget();
                        Ok::<_, std::io::Error>(Bytes::from(result.to_string()))
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            requests,
            release,
            body_dropped,
            _server: AbortOnDropHandle::new(server),
        }
    }

    fn running(&self, delay: u64) -> Running {
        self.running_with_owners(delay, false)
    }

    fn running_with_owners(&self, delay: u64, require_owner: bool) -> Running {
        let mut running = create_test_running_with_operation(self.endpoint.clone());
        running.enable_output_schema = true;
        running.headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        running.tasks = Some(TaskRuntime::new(
            TasksConfig {
                enabled: true,
                detach_after_ms: delay,
                ..Default::default()
            },
            running.cancellation_token.clone(),
            require_owner,
        ));
        running
    }
}

fn message(method: &str, mut params: Value, tasks: bool) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "task-test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": if tasks { json!({"extensions":{"io.modelcontextprotocol/tasks":{}}}) } else { json!({}) }
    });
    json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params})
}

async fn http(running: &Running, method: &str, params: Value, tasks: bool) -> Value {
    http_at(running, "/mcp", None, method, params, tasks).await
}

/// A request the auth middleware has already validated for `principal`.
async fn http_as(running: &Running, principal: &Principal, method: &str, params: Value) -> Value {
    http_at(running, "/mcp", Some(principal), method, params, true).await
}

async fn http_at(
    running: &Running,
    uri: &str,
    principal: Option<&Principal>,
    method: &str,
    params: Value,
    tasks: bool,
) -> Value {
    // A new handler is constructed on every request: task ownership must be
    // application-wide, not tied to this handler or the response connection.
    let app = running.clone();
    let service: StreamableHttpService<
        McpService,
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
    > = StreamableHttpService::new(
        move || Ok(app.for_service()),
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true),
    );
    let name = params
        .get("taskId")
        .or_else(|| params.get("name"))
        .and_then(Value::as_str);
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", method)
        .header("x-tenant-id", "tenant-one");
    if let Some(name) = name {
        request = request.header("Mcp-Name", name);
    }
    if let Some(principal) = principal {
        request = request.extension(ValidToken {
            token: headers::Authorization::bearer("token").unwrap(),
            scopes: vec![],
            principal: principal.clone(),
        });
    }
    let response = service
        .oneshot(
            request
                .body(Body::from(message(method, params, tasks).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        response.status(),
        http::StatusCode::OK | http::StatusCode::BAD_REQUEST
    ));
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn terminal(running: &Running, id: &Value) -> Value {
    loop {
        let result = http(running, "tasks/get", json!({"taskId":id}), true).await;
        if result["result"]["status"] != "working" {
            return result["result"].clone();
        }
        tokio::task::yield_now().await;
    }
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn http_detaches_once_and_preserves_results(#[case] graphql_error: bool) {
    let payload = if graphql_error {
        json!({"data":{"hello":null},"errors":[{"message":"unavailable"}]})
    } else {
        json!({"data":{"hello":"world"}})
    };
    let mut upstream = Upstream::new(payload.clone()).await;
    let mut running = upstream.running(1);
    running.forward_headers.push("x-tenant-id".into());
    let discover = http(&running, "server/discover", json!({}), true).await;
    assert_eq!(
        discover["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"],
        json!({})
    );
    let handle = http(&running, "tools/call", json!({"name":"Hello"}), true).await;
    assert_eq!(handle["result"]["resultType"], "task");
    let id = &handle["result"]["taskId"];
    let (headers, request) = upstream.requests.recv().await.unwrap();
    assert_eq!(headers["x-tenant-id"], "tenant-one");
    assert_eq!(request["operationName"], "Hello");
    assert_eq!(
        http(&running, "tasks/get", json!({"taskId":id}), true).await["result"]["status"],
        "working"
    );
    // A catalog reload must not wait for upstream I/O, or change the running operation.
    running.update_operations(vec![]).await;
    upstream.release.add_permits(1);
    let result = terminal(&running, id).await;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["result"]["structuredContent"], payload);
    assert_eq!(result["result"]["isError"], graphql_error);
    assert!(upstream.requests.try_recv().is_err());
    assert_eq!(
        http(&running, "tasks/cancel", json!({"taskId":id}), true).await["result"]["resultType"],
        "complete"
    );
    assert_eq!(terminal(&running, id).await, result);
    assert_eq!(
        http(
            &running,
            "tasks/update",
            json!({"taskId":id,"inputResponses":{"unknown":{}}}),
            true
        )
        .await["result"]["resultType"],
        "complete"
    );
    assert_eq!(
        http(&running, "tasks/get", json!({"taskId":id}), false).await["error"]["code"],
        -32021
    );
    assert_eq!(
        http(&running, "tasks/get", json!({"taskId":"unknown"}), true).await["error"]["code"],
        -32602
    );
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn tasks_are_bound_to_the_creating_principal() {
    let mut upstream = Upstream::new(json!({"data":{"hello":"world"}})).await;
    let running = upstream.running_with_owners(0, true);
    let principal = |subject: &str| Principal {
        issuer: Some("https://idp.example.com".into()),
        subject: subject.into(),
    };
    let (alice, mallory) = (principal("alice"), principal("mallory"));
    let handle = http_as(&running, &alice, "tools/call", json!({"name":"Hello"})).await;
    let id = &handle["result"]["taskId"];
    upstream.requests.recv().await.unwrap();
    let unknown = http_as(&running, &mallory, "tasks/get", json!({"taskId":"unknown"})).await;
    assert!(unknown["error"].is_object());
    for method in ["tasks/get", "tasks/cancel", "tasks/update"] {
        let params = json!({"taskId":id,"inputResponses":{}});
        assert_eq!(
            http_as(&running, &mallory, method, params).await["error"],
            unknown["error"],
            "{method} must not reveal another principal's task"
        );
    }
    upstream.release.add_permits(1);
    let result = loop {
        let result = http_as(&running, &alice, "tasks/get", json!({"taskId":id})).await;
        if result["result"]["status"] != "working" {
            break result;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(result["result"]["status"], "completed");
    assert_eq!(
        result["result"]["result"]["structuredContent"]["data"]["hello"],
        "world"
    );
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn http_cancel_releases_started_upstream_request() {
    let mut upstream = Upstream::new(json!({"data":{"hello":"world"}})).await;
    let running = upstream.running(0);
    let handle = http(&running, "tools/call", json!({"name":"Hello"}), true).await;
    let id = &handle["result"]["taskId"];
    upstream.requests.recv().await.unwrap();
    assert!(!upstream.body_dropped.is_cancelled());
    for _ in 0..2 {
        assert!(
            http(&running, "tasks/cancel", json!({"taskId":id}), true)
                .await
                .get("result")
                .is_some()
        );
    }
    assert_eq!(terminal(&running, id).await["status"], "cancelled");
    upstream.body_dropped.cancelled().await;
    assert!(upstream.requests.try_recv().is_err());
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[case::fast(true, true, 60_000, false)]
#[case::disabled(false, true, 0, false)]
#[case::unsupported(true, false, 0, false)]
#[case::tokenless_with_auth(true, true, 0, true)]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn synchronous_compatibility(
    #[case] enabled: bool,
    #[case] supported: bool,
    #[case] delay: u64,
    #[case] require_owner: bool,
) {
    let upstream = Upstream::new(json!({"data":{"hello":"world"}})).await;
    upstream.release.add_permits(1);
    let mut running = upstream.running_with_owners(delay, require_owner);
    if !enabled {
        running.tasks = None;
    }
    let result = http(&running, "tools/call", json!({"name":"Hello"}), supported).await;
    assert_eq!(
        result["result"]["structuredContent"]["data"]["hello"],
        "world"
    );
    assert!(result["result"].get("taskId").is_none());
    if let Some(tasks) = &running.tasks {
        tasks.shutdown().await;
    }
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn stdio_task_lifecycle() {
    let mut upstream = Upstream::new(json!({"data":{"hello":"world"}})).await;
    let running = upstream.running(0);
    let service = running.for_service();
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        service
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = ClientConfig::new(
        ClientCapabilities::builder().enable_tasks().build(),
        Implementation::new("tasks-test", "1"),
    )
    .serve(client_io)
    .await
    .unwrap();
    let CallToolResponse::Task(handle) = client
        .call_tool_once(CallToolRequestParams::new("Hello"))
        .await
        .unwrap()
    else {
        panic!("expected task")
    };
    upstream.requests.recv().await.unwrap();
    client
        .cancel_task(CancelTaskParams::new(handle.task.task_id.clone()))
        .await
        .unwrap();
    loop {
        let result = client
            .get_task(GetTaskParams::new(handle.task.task_id.clone()))
            .await
            .unwrap();
        if result.task.status() == TaskStatus::Cancelled {
            break;
        }
        tokio::task::yield_now().await;
    }
    upstream.body_dropped.cancelled().await;
    client.cancel().await.unwrap();
    server.await.unwrap();
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[case::query("query Hello { hello }")]
#[case::mutation("mutation Hello { hello }")]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn generic_execute_detaches_supported_operations(#[case] query: &str) {
    let mut upstream = Upstream::new(json!({"data":{"hello":"world"}})).await;
    let mut running = upstream.running(0);
    running.execute_tool = Some(Execute::new(MutationMode::All, None));
    let response = http(
        &running,
        "tools/call",
        json!({"name":"execute","arguments":{"query":query}}),
        true,
    )
    .await;
    assert_eq!(response["result"]["resultType"], "task");
    let (_, request) = upstream.requests.recv().await.unwrap();
    assert_eq!(request["query"], query);
    upstream.release.add_permits(1);
    assert_eq!(
        terminal(&running, &response["result"]["taskId"]).await["result"]["structuredContent"]["data"]
            ["hello"],
        "world"
    );
    assert!(upstream.requests.try_recv().is_err());
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn utility_and_unknown_tools_never_create_tasks() {
    let upstream = Upstream::new(json!({})).await;
    let mut running = upstream.running(0);
    running.validate_tool = Some(Validate::new(running.schema.clone(), None));
    let result = http(
        &running,
        "tools/call",
        json!({"name":"validate","arguments":{"operation":"query Hello { hello }"}}),
        true,
    )
    .await;
    assert!(result["result"].get("taskId").is_none());
    assert!(result.get("result").is_some());
    let result = http(&running, "tools/call", json!({"name":"unknown"}), true).await;
    assert!(result.get("error").is_some());
    assert_eq!(
        running
            .tasks
            .as_ref()
            .unwrap()
            .manager()
            .running_task_count(),
        0
    );
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn capacity_falls_back_to_same_execution_and_shutdown_releases_upstream() {
    let mut upstream = Upstream::new(json!({"data":{"hello":"world"}})).await;
    let mut running = upstream.running(0);
    running.tasks = Some(TaskRuntime::new(
        TasksConfig {
            enabled: true,
            detach_after_ms: 0,
            max_tasks: 1.try_into().unwrap(),
            ..Default::default()
        },
        running.cancellation_token.clone(),
        false,
    ));
    let first = http(&running, "tools/call", json!({"name":"Hello"}), true).await;
    upstream.requests.recv().await.unwrap();
    assert_eq!(first["result"]["resultType"], "task");
    let second_app = running.clone();
    let second = tokio::spawn(async move {
        http(&second_app, "tools/call", json!({"name":"Hello"}), true).await
    });
    upstream.requests.recv().await.unwrap();
    assert!(!second.is_finished());
    upstream.release.add_permits(2);
    assert!(second.await.unwrap()["result"].get("taskId").is_none());
    assert_eq!(
        terminal(&running, &first["result"]["taskId"]).await["status"],
        "completed"
    );
    assert!(upstream.requests.try_recv().is_err());
    running.tasks.as_ref().unwrap().shutdown().await;

    let mut upstream = Upstream::new(json!({})).await;
    let running = upstream.running(0);
    let handle = http(&running, "tools/call", json!({"name":"Hello"}), true).await;
    upstream.requests.recv().await.unwrap();
    assert_eq!(handle["result"]["resultType"], "task");
    running.tasks.as_ref().unwrap().shutdown().await;
    upstream.body_dropped.cancelled().await;
    assert_eq!(
        http(
            &running,
            "tasks/get",
            json!({"taskId":handle["result"]["taskId"]}),
            true
        )
        .await["error"]["code"],
        -32602
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn private_fields_keep_their_original_result_visibility() {
    let payload = json!({"data":{"hello":"world","secret":"private"}});
    let mut upstream = Upstream::new(payload.clone()).await;
    let running = upstream.running(0);
    let schema =
        Schema::parse_and_validate("type Query { hello: String secret: String }", "test").unwrap();
    let operation = RawOperation::from(("query Hello { hello secret @private }".to_owned(), None))
        .into_operation(
            &schema,
            None,
            MutationMode::None,
            false,
            false,
            true,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
    *running.operations.write().await = vec![operation];
    let response = http(&running, "tools/call", json!({"name":"Hello"}), true).await;
    upstream.requests.recv().await.unwrap();
    upstream.release.add_permits(1);
    let result = terminal(&running, &response["result"]["taskId"]).await;
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"data":{"hello":"world"}})
    );
    assert_eq!(result["result"]["_meta"]["structuredContent"], payload);
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn app_task_keeps_scoped_result_shape() {
    use crate::apps::app::{App, AppResource, AppResourceSource, AppTool};
    let mut upstream = Upstream::new(json!({"data":{"hello":"app"}})).await;
    let mut running = upstream.running(0);
    let operation = Arc::new(running.operations.read().await[0].clone());
    running.apps = vec![App {
        name: "TaskApp".into(),
        description: None,
        resource: AppResource::Single(AppResourceSource::Local("test".into())),
        csp_settings: None,
        widget_settings: None,
        uri: "ui://TaskApp".parse().unwrap(),
        tools: vec![AppTool {
            operation,
            labels: Default::default(),
            tool: Tool::new("Hello", "app tool", JsonObject::new()),
            extra_outputs: None,
        }],
        prefetch_operations: vec![],
    }];
    let handle = http_at(
        &running,
        "/mcp?app=TaskApp",
        None,
        "tools/call",
        json!({"name":"Hello"}),
        true,
    )
    .await;
    assert_eq!(handle["result"]["resultType"], "task");
    upstream.requests.recv().await.unwrap();
    upstream.release.add_permits(1);
    let result = terminal(&running, &handle["result"]["taskId"]).await;
    assert_eq!(
        result["result"]["structuredContent"]["result"]["data"]["hello"],
        "app"
    );
    running.tasks.as_ref().unwrap().shutdown().await;
}

#[rstest::rstest]
#[case("tasks/get")]
#[case("tasks/cancel")]
#[case("tasks/update")]
#[tokio::test]
async fn task_methods_keep_endpoint_authentication(#[case] method: &str) {
    let config: crate::auth::Config = serde_json::from_value(json!({
        "servers":["http://localhost:1234"], "audiences":["test"], "resource":"http://localhost/mcp"
    }))
    .unwrap();
    let running = test_support::create_test_running();
    let service =
        crate::server::states::starting::build_http_service(running, false, &Default::default());
    let router = config
        .enable_middleware(
            Router::new().nest_service("/mcp", service),
            HashMap::new(),
            false,
        )
        .unwrap();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("MCP-Protocol-Version", "2026-07-28")
                .header("Mcp-Method", method)
                .header("Mcp-Name", "secret-task")
                .body(Body::from(
                    message(
                        method,
                        json!({"taskId":"secret-task","inputResponses":{}}),
                        true,
                    )
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
}
