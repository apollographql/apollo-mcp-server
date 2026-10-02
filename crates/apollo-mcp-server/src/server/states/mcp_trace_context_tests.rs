//! Exercise metadata propagation through rmcp dispatch and the GraphQL client.

use super::*;
use http_body_util::BodyExt as _;
use opentelemetry::baggage::BaggageExt as _;
use opentelemetry::propagation::TextMapPropagator as _;
use opentelemetry::trace::{SpanId, TraceContextExt as _};
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, PaginatedRequestParams, RequestMetaObject};
use serde_json::{Value, json};
use std::collections::HashMap;
use tracing::instrument::WithSubscriber as _;

const HANDLER_OFF: &str = "info,apollo_mcp_server::server::states::running=off";
const STATES_OFF: &str = "info,apollo_mcp_server::server::states=off";

#[derive(Clone, Copy)]
enum HttpSpan {
    Enabled,
    Filtered,
}

#[derive(Clone, Copy)]
enum HandlerSpans {
    Enabled,
    Filtered,
}

const HTTP_TRACEPARENT: &str = "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01";

fn named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    let matching: Vec<_> = spans.iter().filter(|span| span.name == name).collect();
    assert_eq!(matching.len(), 1, "expected one {name} span: {spans:#?}");
    matching[0]
}

fn assert_downstream_context(spans: &[SpanData], headers: &HashMap<String, String>) {
    let handler = named(spans, "call_tool");
    let execute = named(spans, "execute");
    let client = named(spans, "mcp-graphql-client");
    assert_eq!(execute.parent_span_id, handler.span_context.span_id());
    assert_eq!(client.parent_span_id, execute.span_context.span_id());
    assert_eq!(
        client.span_context.trace_id(),
        handler.span_context.trace_id()
    );
    assert_eq!(client.span_kind, SpanKind::Client);

    let outgoing = w3c_text_map_propagator().extract(headers);
    assert_eq!(
        outgoing.span().span_context().trace_id(),
        client.span_context.trace_id()
    );
    assert_eq!(
        outgoing.span().span_context().span_id(),
        client.span_context.span_id(),
        "forward the active client span, not the caller's traceparent"
    );
    assert_eq!(
        outgoing.span().span_context().trace_state(),
        handler.span_context.trace_state()
    );
}

/// Capture the actual headers at the upstream boundary, without assuming the
/// client span ID in advance or relying on baggage member order.
fn capture_headers(
    request: &mockito::Request,
    tx: &tokio::sync::mpsc::UnboundedSender<HashMap<String, String>>,
) -> bool {
    let headers = ["traceparent", "tracestate", "baggage", "x-hook-trace-id"]
        .into_iter()
        .filter_map(|key| {
            request
                .header(key)
                .first()
                .map(|value| (key.to_owned(), value.to_str().unwrap().to_owned()))
        })
        .collect();
    tx.send(headers).unwrap();
    true
}

async fn http_tool_call(
    meta: Value,
    forward_headers: Vec<String>,
    version: &str,
) -> (Vec<SpanData>, HashMap<String, String>) {
    http_tool_call_with_filter(meta, forward_headers, version, "trace").await
}

async fn http_tool_call_with_filter(
    mut meta: Value,
    forward_headers: Vec<String>,
    version: &str,
    filter: &str,
) -> (Vec<SpanData>, HashMap<String, String>) {
    if version == "2026-07-28" {
        meta["io.modelcontextprotocol/protocolVersion"] = json!(version);
        meta["io.modelcontextprotocol/clientCapabilities"] = json!({});
    }
    let spans = ExportedSpans::capture_with_filter(tracing_subscriber::EnvFilter::new(filter));
    let mut graphql = mockito::Server::new_async().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let endpoint = graphql
        .mock("POST", "/")
        .match_request(move |request| capture_headers(request, &tx))
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":{"hello":"world"}}"#)
        .expect(1)
        .create_async()
        .await;
    let mut running = create_test_running_with_operation(graphql.url().parse().unwrap());
    running.forward_headers = forward_headers;
    let scripts = tempfile::tempdir().unwrap();
    std::fs::write(
        scripts.path().join("main.rhai"),
        r#"
        fn on_execute_graphql_operation(ctx) {
            let headers = ctx.headers;
            headers["x-hook-trace-id"] = ctx.trace_id;
            ctx.headers = headers;
        }
    "#,
    )
    .unwrap();
    running.rhai_engine = apollo_mcp_rhai::SharedRhaiEngine::load(scripts.path()).unwrap();
    let service = build_http_service(running, false, &Default::default());
    let router = with_telemetry_layers(Router::new().nest_service("/mcp", service));
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Host", "localhost:8000")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Protocol-Version", version)
                .header("Mcp-Method", "tools/call")
                .header("Mcp-Name", "Hello")
                .header("traceparent", HTTP_TRACEPARENT)
                .header("tracestate", "http=one")
                .header("baggage", "source=http")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": "Hello", "arguments": {}, "_meta": meta}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    endpoint.assert_async().await;
    let headers = rx.try_recv().expect("GraphQL request was captured");
    let spans = spans.collect();
    (spans, headers)
}

#[tokio::test]
async fn explicit_forwarded_baggage_remains_when_context_baggage_is_empty() {
    let (spans, headers) = http_tool_call(
        json!({"traceparent": TRACEPARENT, "baggage": ""}),
        vec!["baggage".into(), "tracestate".into()],
        "2025-11-25",
    )
    .await;
    let handler = named(&spans, "call_tool");
    assert_eq!(handler.parent_span_id.to_string(), REMOTE_SPAN_ID);
    assert_eq!(handler.span_context.trace_state().header(), "");
    assert_eq!(headers["baggage"], "source=http");
    assert_eq!(headers["tracestate"], "");
    let outgoing = w3c_text_map_propagator().extract(&headers);
    // Trace context still injects: the traceparent belongs to the client span
    // and its empty tracestate replaces the explicitly forwarded value.
    assert_eq!(
        outgoing.span().span_context().span_id(),
        named(&spans, "mcp-graphql-client").span_context.span_id()
    );
}

#[rstest::rstest]
#[case::metadata_wins(json!({"traceparent": TRACEPARENT, "tracestate": "meta=one", "baggage": "source=meta"}), true, Some("source=meta"), "meta=one")]
#[case::absent(json!({}), false, Some("source=http"), "http=one")]
#[case::invalid_parent(json!({"traceparent": "invalid", "tracestate": "meta=one"}), false, Some("source=http"), "http=one")]
#[case::non_string(json!({"traceparent": 42, "tracestate": [], "baggage": false}), false, Some("source=http"), "http=one")]
#[case::baggage_only(json!({"baggage": "source=meta"}), false, Some("source=meta"), "http=one")]
#[case::clear_baggage(json!({"baggage": ""}), false, None, "http=one")]
#[case::invalid_baggage(json!({"baggage": "invalid"}), false, None, "http=one")]
#[case::unsafe_baggage(json!({"baggage": "safe=ok,userId=alice;property=one%0Atwo"}), false, Some("safe=ok"), "http=one")]
#[case::retain_http_baggage(json!({"traceparent": TRACEPARENT}), true, Some("source=http"), "")]
#[case::invalid_state(json!({"traceparent": TRACEPARENT, "tracestate": "invalid"}), true, Some("source=http"), "")]
#[case::control_char_state(json!({"traceparent": TRACEPARENT, "tracestate": "meta=one,vendor=k\u{1}"}), true, Some("source=http"), "")]
#[case::non_ascii_state(json!({"traceparent": TRACEPARENT, "tracestate": "vendor=caf\u{e9}"}), true, Some("source=http"), "")]
#[case::raw_equals(json!({"baggage": "token=a=b"}), false, Some("token=a%3Db"), "http=one")]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn http_metadata_context_reaches_graphql(
    #[case] meta: Value,
    #[case] metadata_parent: bool,
    #[case] expected_baggage: Option<&str>,
    #[case] expected_state: &str,
    #[values("2025-11-25", "2026-07-28")] version: &str,
) {
    let (spans, headers) = http_tool_call(meta, vec![], version).await;
    let http = named(&spans, "POST /mcp");
    let handler = named(&spans, "call_tool");
    assert_eq!(http.parent_span_id.to_string(), "bbbbbbbbbbbbbbbb");
    assert_eq!(
        http.span_context.trace_id().to_string(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    if metadata_parent {
        assert_eq!(handler.parent_span_id.to_string(), REMOTE_SPAN_ID);
        assert_eq!(handler.span_context.trace_id().to_string(), REMOTE_TRACE_ID);
    } else {
        assert_eq!(handler.parent_span_id, http.span_context.span_id());
        assert_eq!(
            handler.span_context.trace_id(),
            http.span_context.trace_id()
        );
    }
    assert_eq!(handler.span_context.trace_state().header(), expected_state);
    assert_eq!(headers.get("baggage").map(String::as_str), expected_baggage);
    assert_downstream_context(&spans, &headers);
    assert_eq!(
        headers["x-hook-trace-id"],
        handler.span_context.trace_id().to_string()
    );
}

#[rstest::rstest]
#[case::metadata_handler_filtered(json!({"traceparent": TRACEPARENT, "tracestate": "meta=one", "baggage": "source=meta"}), true, Some("source=meta"), HANDLER_OFF, HttpSpan::Enabled)]
#[case::metadata_all_filtered(json!({"traceparent": TRACEPARENT, "tracestate": "meta=one", "baggage": "source=meta"}), true, Some("source=meta"), STATES_OFF, HttpSpan::Filtered)]
#[case::absent_handler_filtered(json!({}), false, Some("source=http"), HANDLER_OFF, HttpSpan::Enabled)]
#[case::absent_all_filtered(json!({}), false, Some("source=http"), STATES_OFF, HttpSpan::Filtered)]
#[case::invalid_handler_filtered(json!({"traceparent": "invalid", "tracestate": "meta=one"}), false, Some("source=http"), HANDLER_OFF, HttpSpan::Enabled)]
#[case::invalid_all_filtered(json!({"traceparent": "invalid", "tracestate": "meta=one"}), false, Some("source=http"), STATES_OFF, HttpSpan::Filtered)]
#[case::clear_baggage_handler_filtered(json!({"traceparent": TRACEPARENT, "baggage": ""}), true, None, HANDLER_OFF, HttpSpan::Enabled)]
#[case::clear_baggage_all_filtered(json!({"traceparent": TRACEPARENT, "baggage": ""}), true, None, STATES_OFF, HttpSpan::Filtered)]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn filtered_handler_preserves_request_context(
    #[case] meta: Value,
    #[case] metadata_parent: bool,
    #[case] expected_baggage: Option<&str>,
    #[values("2025-11-25", "2026-07-28")] version: &str,
    #[case] filter: &str,
    #[case] http_span: HttpSpan,
) {
    let (spans, headers) = http_tool_call_with_filter(meta, vec![], version, filter).await;
    assert!(!spans.iter().any(|span| span.name == "call_tool"));
    let execute = named(&spans, "execute");
    let client = named(&spans, "mcp-graphql-client");
    let trace_id = if metadata_parent {
        REMOTE_TRACE_ID
    } else {
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    };
    assert_eq!(execute.span_context.trace_id().to_string(), trace_id);
    let expected_parent = if metadata_parent {
        REMOTE_SPAN_ID.to_owned()
    } else {
        match http_span {
            HttpSpan::Enabled => named(&spans, "POST /mcp")
                .span_context
                .span_id()
                .to_string(),
            HttpSpan::Filtered => "bbbbbbbbbbbbbbbb".to_owned(),
        }
    };
    assert_eq!(execute.parent_span_id.to_string(), expected_parent);
    assert_eq!(client.parent_span_id, execute.span_context.span_id());
    let outgoing = w3c_text_map_propagator().extract(&headers);
    assert_eq!(
        outgoing.span().span_context().trace_id().to_string(),
        trace_id
    );
    assert_eq!(
        outgoing.span().span_context().span_id(),
        client.span_context.span_id()
    );
    assert_eq!(
        outgoing.span().span_context().trace_state(),
        execute.span_context.trace_state()
    );
    assert_eq!(headers.get("baggage").map(String::as_str), expected_baggage);
    assert_eq!(headers["x-hook-trace-id"], trace_id);
    match http_span {
        HttpSpan::Enabled => {
            named(&spans, "POST /mcp");
        }
        HttpSpan::Filtered => {
            assert!(!spans.iter().any(|span| span.span_kind == SpanKind::Server));
        }
    }
}

#[tokio::test]
async fn initialize_metadata_parents_the_handler_without_reparenting_http() {
    let spans = ExportedSpans::capture();
    let mut request = mcp_request("/mcp");
    *request.body_mut() = Body::from(
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0.0"},
                "_meta": {"traceparent": TRACEPARENT}
            }
        })
        .to_string(),
    );
    let router = production_router(create_test_running());
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session = response.headers()["mcp-session-id"].clone();
    response.into_body().collect().await.unwrap();
    // Delete the actual session before collecting its spans.
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("Host", "localhost:8000")
                .header("mcp-session-id", session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    response.into_body().collect().await.unwrap();
    let spans = spans.collect();
    assert_eq!(
        named(&spans, "initialize").parent_span_id.to_string(),
        REMOTE_SPAN_ID
    );
    assert_eq!(named(&spans, "POST /mcp").parent_span_id, SpanId::INVALID);
}

fn tool_request(traceparent: &str, baggage: &str) -> CallToolRequestParams {
    let mut meta = RequestMetaObject::new();
    meta.set_traceparent(traceparent);
    meta.set_tracestate("meta=one");
    meta.set_baggage(baggage);
    let mut request = CallToolRequestParams::new("Hello").with_arguments(Default::default());
    request.meta = Some(meta);
    request
}

#[rstest::rstest]
#[case::handler_spans_enabled("trace", HandlerSpans::Enabled)]
#[case::handler_spans_filtered(HANDLER_OFF, HandlerSpans::Filtered)]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn stdio_requests_have_independent_contexts_and_propagate_downstream(
    #[case] filter: &str,
    #[case] handler_spans: HandlerSpans,
) {
    let spans = ExportedSpans::capture_with_filter(tracing_subscriber::EnvFilter::new(filter));
    let mut graphql = mockito::Server::new_async().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let endpoint = graphql
        .mock("POST", "/")
        .match_request(move |request| capture_headers(request, &tx))
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":{"hello":"world"}}"#)
        .expect(3)
        .create_async()
        .await;
    let running = create_test_running_with_operation(graphql.url().parse().unwrap());
    let service = running.for_service();
    let (server_io, client_io) = tokio::io::duplex(4096);
    let server = tokio::spawn(
        async move { service.serve(server_io).await.unwrap() }.with_current_subscriber(),
    );
    let client = ().serve(client_io).await.unwrap();
    let server = server.await.unwrap();
    // The SDK client completes discovery before exposing its modern peer.
    let second_parent = "00-cccccccccccccccccccccccccccccccc-dddddddddddddddd-01";
    let (first, second) = tokio::join!(
        client
            .peer()
            .call_tool(tool_request(TRACEPARENT, "source=first")),
        client
            .peer()
            .call_tool(tool_request(second_parent, "source=second")),
    );
    assert_ne!(first.unwrap().is_error, Some(true));
    assert_ne!(second.unwrap().is_error, Some(true));
    let third = client
        .peer()
        .call_tool(CallToolRequestParams::new("Hello").with_arguments(Default::default()))
        .await
        .unwrap();
    assert_ne!(third.is_error, Some(true));
    let mut params = PaginatedRequestParams::default();
    let mut meta = RequestMetaObject::new();
    meta.set_traceparent(TRACEPARENT);
    params.meta = Some(meta);
    client.peer().list_resources(Some(params)).await.unwrap();
    client.cancel().await.unwrap();
    server.cancel().await.unwrap();
    endpoint.assert_async().await;
    let headers: Vec<_> = (0..3).map(|_| rx.try_recv().unwrap()).collect();
    let spans = spans.collect();
    let handler_name = match handler_spans {
        HandlerSpans::Enabled => "call_tool",
        HandlerSpans::Filtered => "execute",
    };
    let handlers: Vec<_> = spans
        .iter()
        .filter(|span| span.name == handler_name)
        .collect();
    assert_eq!(handlers.len(), 3);
    for (trace_id, parent_id, baggage) in [
        (REMOTE_TRACE_ID, REMOTE_SPAN_ID, "first"),
        (
            "cccccccccccccccccccccccccccccccc",
            "dddddddddddddddd",
            "second",
        ),
    ] {
        let handler = handlers
            .iter()
            .find(|span| span.span_context.trace_id().to_string() == trace_id)
            .unwrap();
        assert_eq!(handler.parent_span_id.to_string(), parent_id);
        let headers = headers
            .iter()
            .find(|headers| headers["traceparent"].contains(trace_id))
            .unwrap();
        let outgoing = w3c_text_map_propagator().extract(headers);
        assert_eq!(outgoing.baggage().get("source").unwrap().as_str(), baggage);
        let trace_spans: Vec<_> = spans
            .iter()
            .filter(|span| span.span_context.trace_id() == handler.span_context.trace_id())
            .cloned()
            .collect();
        // list_resources shares the first remote trace, but is not a tool span.
        match handler_spans {
            HandlerSpans::Enabled => assert_downstream_context(&trace_spans, headers),
            HandlerSpans::Filtered => {
                let client_span = named(&trace_spans, "mcp-graphql-client");
                assert_eq!(client_span.parent_span_id, handler.span_context.span_id());
                assert_eq!(
                    outgoing.span().span_context().span_id(),
                    client_span.span_context.span_id()
                );
            }
        }
    }
    let root = handlers
        .iter()
        .find(|span| span.parent_span_id == SpanId::INVALID)
        .expect("untraced request starts its own root");
    let root_headers = headers
        .iter()
        .find(|headers| headers["traceparent"].contains(&root.span_context.trace_id().to_string()))
        .unwrap();
    assert!(!root_headers.contains_key("baggage"));
    assert_eq!(root.span_context.trace_state().header(), "");
    match handler_spans {
        HandlerSpans::Filtered => {
            assert!(
                !spans
                    .iter()
                    .any(|span| span.name == "call_tool" || span.name == "list_resources")
            );
        }
        HandlerSpans::Enabled => {
            let resource = named(&spans, "list_resources");
            assert_eq!(resource.parent_span_id.to_string(), REMOTE_SPAN_ID);
            assert_eq!(
                resource.span_context.trace_id().to_string(),
                REMOTE_TRACE_ID
            );
        }
    }
}

/// Modern HTTP carries protocol selection on each request, independently of discovery.
fn metadata_request(id: u32, method: &str, mut params: Value) -> Request<Body> {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "traceparent": TRACEPARENT
    });
    let name = params
        .get("name")
        .or_else(|| params.get("uri"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("Mcp-Name", name)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", method)
        .header("traceparent", HTTP_TRACEPARENT)
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        ))
        .unwrap()
}

#[rstest::rstest]
#[case::tools("tools/list", json!({}), "list_tools")]
#[case::resources("resources/list", json!({}), "list_resources")]
#[case::read_resource("resources/read", json!({"uri": "file:///missing"}), "read_resource")]
#[case::prompts("prompts/list", json!({}), "list_prompts")]
#[case::get_prompt("prompts/get", json!({"name": "missing"}), "get_prompt")]
#[case::set_level("logging/setLevel", json!({"level": "debug"}), "set_level")]
#[tokio::test]
async fn metadata_parents_each_http_handler(
    #[case] method: &str,
    #[case] params: Value,
    #[case] name: &str,
) {
    let spans = ExportedSpans::capture();
    let running = create_test_running();
    let service = build_http_service(running, false, &Default::default());
    let router = with_telemetry_layers(Router::new().nest_service("/mcp", service));
    let response = router
        .oneshot(metadata_request(1, method, params))
        .await
        .unwrap();
    response.into_body().collect().await.unwrap();
    // Missing named resources/prompts and modern setLevel return errors, but
    // still dispatch their real handlers and must retain request parentage.
    let spans = spans.collect();
    let handler = named(&spans, name);
    assert_eq!(handler.parent_span_id.to_string(), REMOTE_SPAN_ID);
    assert_eq!(handler.span_context.trace_id().to_string(), REMOTE_TRACE_ID);
    assert_eq!(
        named(&spans, "POST /mcp").parent_span_id.to_string(),
        "bbbbbbbbbbbbbbbb"
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(10))]
async fn metadata_parents_http_subscription_through_readiness_and_cancellation() {
    use super::super::running::test_support::{SseReader, next_message};
    let spans = ExportedSpans::capture();
    let running = create_test_running();
    let service = build_http_service(running.clone(), false, &Default::default());
    let router = with_telemetry_layers(Router::new().nest_service("/mcp", service));
    let response = router
        .clone()
        .oneshot(metadata_request(1, "server/discover", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    let response = router
        .oneshot(metadata_request(
            2,
            "subscriptions/listen",
            json!({"notifications": {"toolsListChanged": true}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut reader = SseReader::new(response.into_body());
    assert_eq!(
        next_message(&mut reader).await["method"],
        "notifications/subscriptions/acknowledged"
    );
    assert_eq!(
        next_message(&mut reader).await["method"],
        "notifications/tools/list_changed"
    );
    assert_eq!(running.tool_list_changes.receiver_count(), 1);
    // Cancel only after the initial refresh proves the production handler is running.
    running.cancellation_token.cancel();
    drop(reader);
    running.tool_list_changes.closed().await;
    let spans = spans.collect();
    let handler = named(&spans, "listen");
    assert_eq!(handler.parent_span_id.to_string(), REMOTE_SPAN_ID);
    assert_eq!(handler.span_context.trace_id().to_string(), REMOTE_TRACE_ID);
    let http_spans: Vec<_> = spans
        .iter()
        .filter(|span| span.name == "POST /mcp")
        .collect();
    assert_eq!(
        http_spans.len(),
        2,
        "discovery and listen each export an HTTP span"
    );
    for http in http_spans {
        assert_eq!(http.parent_span_id.to_string(), "bbbbbbbbbbbbbbbb");
    }
}

#[derive(Clone, Default)]
struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn formatted_handler_logs_retain_http_scope_with_metadata_otel_parent() {
    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = registry()
        .with(tracing_subscriber::EnvFilter::new("debug"))
        .with(OpenTelemetryLayer::new(provider.tracer("test")))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .without_time()
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
                .with_writer(move || writer.clone()),
        );
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let router = production_router(create_test_running());
    let response = router.clone().oneshot(mcp_request("/mcp")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session = response.headers()["mcp-session-id"].clone();
    response.into_body().collect().await.unwrap();
    let mut request = mcp_request("/mcp");
    request
        .headers_mut()
        .insert("mcp-session-id", session.clone());
    *request.body_mut() = Body::from(
        json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        })
        .to_string(),
    );
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    response.into_body().collect().await.unwrap();
    let mut request = mcp_request("/mcp");
    request
        .headers_mut()
        .insert("mcp-session-id", session.clone());
    request
        .headers_mut()
        .insert("traceparent", HTTP_TRACEPARENT.parse().unwrap());
    *request.body_mut() = Body::from(
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "logging/setLevel",
            "params": {"level": "debug", "_meta": {"traceparent": TRACEPARENT}}
        })
        .to_string(),
    );
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("Host", "localhost")
                .header("mcp-session-id", session.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    response.into_body().collect().await.unwrap();
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let handler = named(&spans, "set_level");
    assert_eq!(handler.parent_span_id.to_string(), REMOTE_SPAN_ID);
    assert_eq!(handler.span_context.trace_id().to_string(), REMOTE_TRACE_ID);
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let handler_event = logs
        .lines()
        .find(|line| line.contains("received logging/setLevel; no-op"))
        .expect("real handler event was formatted");
    assert!(
        handler_event.contains("POST /mcp")
            && handler_event.contains("url.path=\"/mcp\"")
            && handler_event.contains("set_level"),
        "handler lost its HTTP tracing scope: {handler_event}"
    );
    // Session attribution happens after the response is accepted, so it is
    // available on HTTP close events, after the handler event has been emitted.
    assert!(
        logs.lines().any(|line| line.contains("POST /mcp")
            && line.contains("apollo.mcp.session_id=")
            && line.contains(session.to_str().unwrap())
            && line.contains("close")),
        "HTTP close lost accepted session scope: {logs}"
    );
}
