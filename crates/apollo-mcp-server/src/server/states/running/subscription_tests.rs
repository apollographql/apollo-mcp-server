//! Exercise the future protocol through rmcp without lifting the production cap.

use axum::body::Body;
use http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::{
    ServiceExt as _,
    model::SubscriptionFilter,
    service::SubscriptionContext,
    transport::{StreamableHttpServerConfig, StreamableHttpService},
};
use serde_json::json;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader, Lines,
};
use tokio_util::task::AbortOnDropHandle;
use tower::ServiceExt as _;

use super::{
    test_support::{SseReader, create_test_running, next_message},
    *,
};

struct FutureProtocol(McpService);

impl ServerHandler for FutureProtocol {
    fn get_info(&self) -> ServerInfo {
        self.0
            .get_info()
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![
            ProtocolVersion::V_2026_07_28,
            ProtocolVersion::V_2025_11_25,
        ])
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        self.0.accepted_subscription_filter(requested)
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        self.0.listen(context).await
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.0.list_tools(request, context).await
    }
}

fn message(id: u32, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "subscription-test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

async fn write_message(writer: &mut (impl AsyncWrite + Unpin), message: Value) {
    writer
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}

async fn read_message(reader: &mut Lines<impl AsyncBufRead + Unpin>) -> Value {
    let line = reader
        .next_line()
        .await
        .unwrap()
        .expect("unexpected stdio EOF");
    serde_json::from_str(&line).expect("invalid JSON-RPC message")
}

async fn cancel_subscription(writer: &mut (impl AsyncWrite + Unpin), id: u32) {
    write_message(
        writer,
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}}),
    )
    .await;
}

fn request(id: u32, method: &str, params: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", method)
        .body(Body::from(message(id, method, params).to_string()))
        .unwrap()
}

fn service(
    running: &Running,
    legacy_sessions: bool,
) -> StreamableHttpService<FutureProtocol, LocalSessionManager> {
    let running = running.clone();
    StreamableHttpService::new(
        move || Ok(FutureProtocol(running.for_service())),
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(legacy_sessions)
            .with_json_response(true),
    )
}

async fn subscribe(running: &Running, id: u32, filter: Value) -> SseReader {
    let response = service(running, false)
        .oneshot(request(
            id,
            "subscriptions/listen",
            json!({"notifications": filter}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("mcp-session-id"));
    let mut reader = SseReader::new(Body::new(response.into_body()));
    let ack = next_message(&mut reader).await;
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
    assert_eq!(
        ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        id
    );
    let accepted = if filter["toolsListChanged"] == true {
        json!({"toolsListChanged": true})
    } else {
        json!({})
    };
    assert_eq!(ack["params"]["notifications"], accepted);
    reader
}

fn assert_change(value: &Value, id: u32) {
    assert_eq!(value["method"], "notifications/tools/list_changed");
    assert_eq!(
        value["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        id
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn reloads_reach_each_subscription_after_catalog_commit() {
    let running = create_test_running();
    let mut first = subscribe(&running, 11, json!({"toolsListChanged": true})).await;
    let mut second = subscribe(
        &running,
        12,
        json!({"toolsListChanged": true, "resourcesListChanged": true}),
    )
    .await;
    assert_change(&next_message(&mut first).await, 11);
    assert_change(&next_message(&mut second).await, 12);
    assert_eq!(running.tool_list_changes.receiver_count(), 2);

    running
        .update_operations(vec![("query Hello { hello }".to_owned(), None).into()])
        .await;
    assert_change(&next_message(&mut first).await, 11);
    assert_change(&next_message(&mut second).await, 12);
    let response = service(&running, false)
        .oneshot(request(20, "tools/list", json!({})))
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let catalog: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(catalog["result"]["tools"][0]["name"], "Hello");

    running
        .update_schema(Schema::parse_and_validate("type Query { hello: Int }", "test").unwrap())
        .await;
    assert_change(&next_message(&mut first).await, 11);
    assert_change(&next_message(&mut second).await, 12);
    let response = service(&running, false)
        .oneshot(request(21, "tools/list", json!({})))
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let catalog: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        catalog["result"]["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("Int"),
        "schema reload updates the tool's return type description: {catalog}"
    );
    drop(first);
    drop(second);
    running.tool_list_changes.closed().await;
}

#[rstest::rstest]
#[case::omitted(json!({}))]
#[case::disabled(json!({"toolsListChanged": false}))]
#[case::unsupported(json!({"promptsListChanged": true, "resourcesListChanged": true, "resourceSubscriptions": ["file:///test"]}))]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn empty_accepted_filter_has_no_receiver_or_notifications(#[case] filter: Value) {
    let running = create_test_running();
    let mut reader = subscribe(&running, 1, filter).await;
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
    running.update_operations(vec![]).await;
    running.cancellation_token.cancel();
    let completion = next_message(&mut reader).await;
    assert_eq!(completion["id"], 1);
    assert_eq!(completion["result"]["resultType"], "complete");
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn disconnect_and_shutdown_release_established_receivers() {
    let running = create_test_running();
    let mut reader = subscribe(&running, 1, json!({"toolsListChanged": true})).await;
    assert_change(&next_message(&mut reader).await, 1);
    assert_eq!(running.tool_list_changes.receiver_count(), 1);
    drop(reader);
    running.tool_list_changes.closed().await;
    let mut reader = subscribe(&running, 2, json!({"toolsListChanged": true})).await;
    assert_change(&next_message(&mut reader).await, 2);
    running.cancellation_token.cancel();
    let completion = next_message(&mut reader).await;
    assert_eq!(completion["result"]["resultType"], "complete");
    running.tool_list_changes.closed().await;
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn modern_get_cannot_open_a_notification_stream(#[case] legacy_sessions: bool) {
    let running = create_test_running();
    let response = service(&running, legacy_sessions)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header("Host", "localhost")
                .header("Accept", "text/event-stream")
                .header("MCP-Protocol-Version", "2026-07-28")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn production_still_rejects_future_protocol_subscriptions() {
    let running = create_test_running();
    let application = running.clone();
    let service: StreamableHttpService<McpService, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(application.for_service()),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
    let response = service
        .oneshot(request(
            1,
            "subscriptions/listen",
            json!({"notifications": {"toolsListChanged": true}}),
        ))
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["error"]["code"],
        ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0,
        "{response}"
    );
    assert!(response.get("result").is_none());
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn stdio_cancellation_targets_one_of_multiple_subscriptions() {
    struct ObservedListener {
        inner: FutureProtocol,
        cancelled_listener_dropped: CancellationToken,
    }
    impl ServerHandler for ObservedListener {
        fn get_info(&self) -> ServerInfo {
            self.inner.get_info()
        }
        fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
            self.inner.supported_protocol_versions()
        }
        fn accepted_subscription_filter(
            &self,
            requested: &SubscriptionFilter,
        ) -> Option<SubscriptionFilter> {
            self.inner.accepted_subscription_filter(requested)
        }
        async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
            // Observe both normal return and rmcp dropping the cancelled
            // request future. The inner future (and receiver) drops first.
            let _ended = (context.sink().id() == &rmcp::model::RequestId::Number(2))
                .then(|| self.cancelled_listener_dropped.clone().drop_guard());
            self.inner.listen(context).await
        }
    }
    let running = create_test_running();
    let cancelled_listener_dropped = CancellationToken::new();
    let handler = ObservedListener {
        inner: FutureProtocol(running.for_service()),
        cancelled_listener_dropped: cancelled_listener_dropped.clone(),
    };
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        handler
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    }));
    let (read, mut write) = tokio::io::split(client_io);
    let mut reader = BufReader::new(read).lines();
    write_message(&mut write, message(1, "server/discover", json!({}))).await;
    let response = read_message(&mut reader).await;
    assert!(response.get("result").is_some());
    for id in [2, 3] {
        write_message(
            &mut write,
            message(
                id,
                "subscriptions/listen",
                json!({"notifications": {"toolsListChanged": true}}),
            ),
        )
        .await;
        let ack = read_message(&mut reader).await;
        assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
        let change = read_message(&mut reader).await;
        assert_change(&change, id);
    }
    assert_eq!(running.tool_list_changes.receiver_count(), 2);
    cancel_subscription(&mut write, 2).await;
    cancelled_listener_dropped.cancelled().await;
    // No reload or connection teardown has occurred: only the cancelled
    // subscription's receiver must have been released while idle.
    assert_eq!(running.tool_list_changes.receiver_count(), 1);
    running.update_operations(vec![]).await;
    let change = read_message(&mut reader).await;
    assert_change(&change, 3);
    drop(write);
    drop(reader);
    server.await.unwrap();
    running.tool_list_changes.closed().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn initial_refresh_covers_reload_after_acknowledgement_before_registration() {
    struct DelayedListener {
        inner: FutureProtocol,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl ServerHandler for DelayedListener {
        fn get_info(&self) -> ServerInfo {
            self.inner.get_info()
        }
        fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
            self.inner.supported_protocol_versions()
        }
        fn accepted_subscription_filter(
            &self,
            requested: &SubscriptionFilter,
        ) -> Option<SubscriptionFilter> {
            self.inner.accepted_subscription_filter(requested)
        }
        async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
            self.entered.notify_one();
            self.release.notified().await;
            self.inner.listen(context).await
        }
    }
    let running = create_test_running();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let application = running.clone();
    let listener_entered = entered.clone();
    let listener_release = release.clone();
    let service: StreamableHttpService<DelayedListener, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(DelayedListener {
                    inner: FutureProtocol(application.for_service()),
                    entered: listener_entered.clone(),
                    release: listener_release.clone(),
                })
            },
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
    let response = service
        .oneshot(request(
            1,
            "subscriptions/listen",
            json!({"notifications": {"toolsListChanged": true}}),
        ))
        .await
        .unwrap();
    let mut reader = SseReader::new(Body::new(response.into_body()));
    assert_eq!(
        next_message(&mut reader).await["method"],
        "notifications/subscriptions/acknowledged"
    );
    entered.notified().await;
    assert_eq!(running.tool_list_changes.receiver_count(), 0);
    running
        .update_operations(vec![("query Hello { hello }".to_owned(), None).into()])
        .await;
    release.notify_one();
    assert_change(&next_message(&mut reader).await, 1);
    assert_eq!(running.tool_list_changes.receiver_count(), 1);
    drop(reader);
    running.tool_list_changes.closed().await;
}

#[derive(Clone, Copy)]
enum BlockedDeliveryEnd {
    Recover,
    Shutdown,
    Cancel,
    Disconnect,
}

#[rstest::rstest]
#[case::recover(BlockedDeliveryEnd::Recover)]
#[case::shutdown(BlockedDeliveryEnd::Shutdown)]
#[case::cancel(BlockedDeliveryEnd::Cancel)]
#[case::disconnect(BlockedDeliveryEnd::Disconnect)]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn blocked_stdio_delivery_does_not_block_reload_or_other_clients(
    #[case] end: BlockedDeliveryEnd,
) {
    use super::test_support::ObservedWriter;
    use std::sync::atomic::{AtomicBool, Ordering};

    let running = create_test_running();
    let handler = FutureProtocol(running.for_service());
    let (server_input, mut input) = tokio::io::duplex(8192);
    let (server_output, output) = tokio::io::duplex(8);
    let armed = Arc::new(AtomicBool::new(false));
    let (blocked, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let writer = ObservedWriter {
        output: server_output,
        armed: armed.clone(),
        blocked,
    };
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        handler
            .serve((server_input, writer))
            .await
            .unwrap()
            .waiting()
            .await
    }));
    let mut reader = BufReader::new(output).lines();
    write_message(&mut input, message(1, "server/discover", json!({}))).await;
    assert!(read_message(&mut reader).await.get("result").is_some());
    write_message(
        &mut input,
        message(
            2,
            "subscriptions/listen",
            json!({"notifications": {"toolsListChanged": true}}),
        ),
    )
    .await;
    assert_eq!(
        read_message(&mut reader).await["method"],
        "notifications/subscriptions/acknowledged"
    );
    let initial = read_message(&mut reader).await;
    assert_change(&initial, 2);

    let mut fast = subscribe(&running, 3, json!({"toolsListChanged": true})).await;
    assert_change(&next_message(&mut fast).await, 3);
    armed.store(true, Ordering::SeqCst);
    running.update_operations(vec![]).await;
    observed.recv().await.unwrap(); // The slow stream has actually blocked.
    assert_change(&next_message(&mut fast).await, 3);
    for _ in 0..3 {
        running.update_operations(vec![]).await;
        assert_change(&next_message(&mut fast).await, 3);
    }
    assert_eq!(running.tool_list_changes.receiver_count(), 2);
    drop(fast);
    match end {
        BlockedDeliveryEnd::Shutdown => {
            running.cancellation_token.cancel();
            // Must release the receiver even while output is still unread.
            running.tool_list_changes.closed().await;
        }
        BlockedDeliveryEnd::Cancel => {
            cancel_subscription(&mut input, 2).await;
            running.tool_list_changes.closed().await;
        }
        BlockedDeliveryEnd::Disconnect => {
            // Close only the output side to fail the outstanding write, while
            // the input side remains open. Teardown cannot rely on input EOF.
            drop(reader);
            running.tool_list_changes.closed().await;
            drop(input);
            let _ = server.await.unwrap();
            return;
        }
        BlockedDeliveryEnd::Recover => {
            for _ in 0..2 {
                let notification = read_message(&mut reader).await;
                assert_change(&notification, 2);
            }
            // A list response proves the stream recovered and that the three
            // updates during the stalled send became only one follow-up change.
            write_message(&mut input, message(4, "tools/list", json!({}))).await;
            let response = read_message(&mut reader).await;
            assert_eq!(response["id"], 4);
        }
    }
    drop(reader);
    drop(input);
    let _ = server.await.unwrap();
    running.tool_list_changes.closed().await;
}
