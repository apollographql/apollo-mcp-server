//! Real-transport pressure tests. Observers report readiness without altering I/O.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use axum::body::Body;
use http::{Request, StatusCode};
use rmcp::{
    Peer, ServiceExt as _,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::{SessionManager, local::LocalSessionManager},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    sync::mpsc,
};
use tokio_util::task::AbortOnDropHandle;
use tower::ServiceExt as _;

use super::test_support::{ObservedWriter, SseReader, create_test_running, next_message};
use super::*;

struct ObservedService {
    inner: McpService,
    initialized: mpsc::UnboundedSender<Peer<RoleServer>>,
}
impl ServerHandler for ObservedService {
    fn get_info(&self) -> ServerInfo {
        self.inner.get_info()
    }
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        self.inner.initialize(request, context).await
    }
    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        let peer = context.peer.clone();
        self.inner.on_initialized(context).await;
        self.initialized.send(peer).unwrap();
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.inner.list_tools(request, context).await
    }
}

fn initialize_message() -> Value {
    serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params": {
        "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"pressure-test", "version":"1"}
    }})
}
async fn write_message(writer: &mut DuplexStream, message: Value) {
    writer
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}
async fn read_message(reader: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    assert_ne!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "unexpected stdio EOF"
    );
    serde_json::from_str(&line).unwrap()
}

struct StdioClient {
    input: DuplexStream,
    output: BufReader<DuplexStream>,
    server: rmcp::service::RunningService<RoleServer, ObservedService>,
    blocked: mpsc::UnboundedReceiver<()>,
}
async fn connect_stdio(running: &Running) -> StdioClient {
    let (server_input, mut input) = tokio::io::duplex(4096);
    let (server_output, output) = tokio::io::duplex(8);
    let (blocked, observed) = mpsc::unbounded_channel();
    let armed = Arc::new(AtomicBool::new(false));
    let (initialized, mut ready) = mpsc::unbounded_channel();
    let handler = ObservedService {
        inner: running.for_service(),
        initialized,
    };
    let writer = ObservedWriter {
        output: server_output,
        armed: armed.clone(),
        blocked,
    };
    let startup = AbortOnDropHandle::new(tokio::spawn(async move {
        handler.serve((server_input, writer)).await.unwrap()
    }));
    let mut output = BufReader::new(output);
    write_message(&mut input, initialize_message()).await;
    assert!(read_message(&mut output).await.get("result").is_some());
    write_message(
        &mut input,
        serde_json::json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
    )
    .await;
    ready.recv().await.unwrap();
    let server = startup.await.unwrap();
    armed.store(true, Ordering::SeqCst);
    StdioClient {
        input,
        output,
        server,
        blocked: observed,
    }
}
fn update(running: &Running, operations: Vec<RawOperation>) -> AbortOnDropHandle<()> {
    let running = running.clone();
    AbortOnDropHandle::new(tokio::spawn(async move {
        running.update_operations(operations).await;
    }))
}

#[rstest::rstest]
#[case::brief(Duration::ZERO)]
#[case::past_old_cutoff(Duration::from_secs(6))]
#[tokio::test(start_paused = true)]
#[timeout(Duration::from_secs(15))]
async fn stdio_recovers_from_actual_write_backpressure(#[case] stall: Duration) {
    let running = create_test_running();
    let mut client = connect_stdio(&running).await;
    let first = update(&running, vec![]);
    client.blocked.recv().await.unwrap();
    tokio::time::advance(stall).await;
    let second = update(
        &running,
        vec![("query Hello { hello }".to_owned(), None).into()],
    );
    let (messages, mut received) = mpsc::unbounded_channel();
    let reader = AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            let mut line = String::new();
            if client.output.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            messages
                .send(serde_json::from_str::<Value>(&line).unwrap())
                .unwrap();
        }
    }));
    first.await.unwrap();
    second.await.unwrap();
    write_message(
        &mut client.input,
        serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
    )
    .await;
    loop {
        let message = received.recv().await.unwrap();
        if message["id"] == 2 {
            assert_eq!(message["result"]["tools"][0]["name"], "Hello");
            break;
        }
    }
    // The request/response above drains earlier notifications. Require fresh delivery.
    let third = update(&running, vec![]);
    assert_eq!(
        received.recv().await.unwrap()["method"],
        "notifications/tools/list_changed"
    );
    third.await.unwrap();
    drop(client.input);
    client.server.waiting().await.unwrap();
    reader.await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn stdio_full_disconnect_releases_a_blocked_write() {
    let running = create_test_running();
    let mut client = connect_stdio(&running).await;
    let pending = update(&running, vec![]);
    client.blocked.recv().await.unwrap();
    drop(client.output);
    drop(client.input);
    client.server.waiting().await.unwrap();
    pending.await.unwrap();
    assert_delivery_released(&running).await;
}

struct HttpFixture {
    running: Running,
    service: StreamableHttpService<ObservedService, LocalSessionManager>,
    manager: Arc<LocalSessionManager>,
    initialized: mpsc::UnboundedReceiver<Peer<RoleServer>>,
}
impl HttpFixture {
    fn new() -> Self {
        let running = create_test_running();
        let mut manager = LocalSessionManager::default();
        manager.session_config.channel_capacity = 1;
        manager.session_config.sse_retry = None;
        let manager = Arc::new(manager);
        let (initialized, ready) = mpsc::unbounded_channel();
        let application = running.clone();
        let service = StreamableHttpService::new(
            move || {
                Ok(ObservedService {
                    inner: application.for_service(),
                    initialized: initialized.clone(),
                })
            },
            manager.clone(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(true)
                .with_cancellation_token(running.cancellation_token.child_token()),
        );
        Self {
            running,
            service,
            manager,
            initialized: ready,
        }
    }
    async fn connect(&mut self) -> (String, Peer<RoleServer>) {
        let response = self
            .service
            .clone()
            .oneshot(http_request("POST", None, initialize_message()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let session = response.headers()["Mcp-Session-Id"]
            .to_str()
            .unwrap()
            .to_owned();
        let response = self
            .service
            .clone()
            .oneshot(http_request(
                "POST",
                Some(&session),
                serde_json::json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        (session, self.initialized.recv().await.unwrap())
    }
    async fn stream(&self, session: &str) -> SseReader {
        let response = self
            .service
            .clone()
            .oneshot(http_request("GET", Some(session), Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        SseReader::new(Body::new(response.into_body()))
    }
    async fn list(&self, session: &str) -> Value {
        let response = self
            .service
            .clone()
            .oneshot(http_request(
                "POST",
                Some(session),
                serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        next_message(&mut SseReader::new(Body::new(response.into_body()))).await["result"].clone()
    }
    async fn delete(&self, session: &str) {
        let response = self
            .service
            .clone()
            .oneshot(http_request("DELETE", Some(session), Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(
            !self
                .manager
                .has_session(&session.to_owned().into())
                .await
                .unwrap()
        );
    }
}
fn http_request(method: &str, session: Option<&str>, message: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri("/mcp")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    request
        .body(if method == "POST" {
            Body::from(message.to_string())
        } else {
            Body::empty()
        })
        .unwrap()
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn http_backpressure_is_isolated_and_delivery_recovers() {
    let mut fixture = HttpFixture::new();
    let (slow_session, peer) = fixture.connect().await;
    let (fast_session, _) = fixture.connect().await;
    let mut slow = fixture.stream(&slow_session).await;
    let mut fast = fixture.stream(&fast_session).await;
    // A completed send fills the one-slot channel. Without polling the body,
    // the next send cannot finish, irrespective of how rmcp schedules it.
    peer.notify_tool_list_changed().await.unwrap();
    let blocked = peer.notify_tool_list_changed();
    tokio::pin!(blocked);
    assert!(futures::poll!(&mut blocked).is_pending());
    let changed = update(
        &fixture.running,
        vec![("query Hello { hello }".to_owned(), None).into()],
    );
    fast.next_notification().await;
    assert_eq!(
        fixture.list(&fast_session).await["tools"][0]["name"],
        "Hello"
    );
    slow.next_notification().await;
    slow.next_notification().await;
    blocked.await.unwrap();
    // Receive the application notification after the two pressure-setting sends.
    slow.next_notification().await;
    changed.await.unwrap();
    assert_eq!(
        fixture.list(&slow_session).await["tools"][0]["name"],
        "Hello"
    );
    let changed = update(&fixture.running, vec![]);
    fast.next_notification().await;
    slow.next_notification().await;
    changed.await.unwrap();
    drop(slow);
    drop(fast);
    fixture.delete(&slow_session).await;
    fixture.delete(&fast_session).await;
    assert_delivery_released(&fixture.running).await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn closing_a_backpressured_get_allows_reconnection() {
    let mut fixture = HttpFixture::new();
    let (session, peer) = fixture.connect().await;
    let stream = fixture.stream(&session).await;
    peer.notify_tool_list_changed().await.unwrap();
    let blocked = peer.notify_tool_list_changed();
    tokio::pin!(blocked);
    assert!(futures::poll!(&mut blocked).is_pending());
    drop(stream);
    blocked.await.unwrap();
    let mut stream = fixture.stream(&session).await;
    let previous = stream.next_notification().await;
    let changed = update(
        &fixture.running,
        vec![("query Hello { hello }".to_owned(), None).into()],
    );
    assert_ne!(stream.next_notification().await, previous);
    changed.await.unwrap();
    assert_eq!(fixture.list(&session).await["tools"][0]["name"], "Hello");
    drop(stream);
    fixture.delete(&session).await;
    assert_delivery_released(&fixture.running).await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn deleting_a_backpressured_session_finishes_after_get_disconnects() {
    let mut fixture = HttpFixture::new();
    let (session, peer) = fixture.connect().await;
    let stream = fixture.stream(&session).await;
    peer.notify_tool_list_changed().await.unwrap();
    let blocked = peer.notify_tool_list_changed();
    tokio::pin!(blocked);
    assert!(futures::poll!(&mut blocked).is_pending());
    fixture.delete(&session).await;
    // DELETE removes the session ID before the worker finishes. Release the
    // output too, then independently establish that forwarding was torn down.
    drop(stream);
    let _ = blocked.await;
    assert_delivery_released(&fixture.running).await;
}

async fn assert_delivery_released(running: &Running) {
    running.tool_list_changes.closed().await;
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn application_cancellation_releases_forwarder_during_blocked_stdio_write() {
    let running = create_test_running();
    let mut client = connect_stdio(&running).await;
    let pending = update(&running, vec![]);
    client.blocked.recv().await.unwrap();
    running.cancellation_token.cancel();
    assert_delivery_released(&running).await;
    // This establishes application-task cleanup, not cancellation of rmcp's
    // separately owned write. Releasing the I/O allows its teardown to finish.
    drop(client.output);
    drop(client.input);
    client.server.waiting().await.unwrap();
    pending.await.unwrap();
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn application_cancellation_releases_forwarder_with_http_output_full() {
    let mut fixture = HttpFixture::new();
    let (session, peer) = fixture.connect().await;
    let stream = fixture.stream(&session).await;
    peer.notify_tool_list_changed().await.unwrap();
    let pending = update(&fixture.running, vec![]);
    pending.await.unwrap();
    assert_eq!(fixture.running.tool_list_changes.receiver_count(), 1);
    fixture.running.cancellation_token.cancel();
    assert_delivery_released(&fixture.running).await;
    drop(stream);
    fixture.delete(&session).await;
}
