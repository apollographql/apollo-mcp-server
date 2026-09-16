//! Assertions on the spans the production router actually exports.
//!
//! These drive the real transport through the real layer stack and read the
//! result back out of an `InMemorySpanExporter`, so they fail when the
//! exported span kind, name, attributes or parentage drift — not just when the
//! middleware's internals change.

use axum::Router;
use axum::body::Body;
use http::{Request, StatusCode};
use opentelemetry::Value as OtelValue;
use opentelemetry::trace::{SpanKind, Status, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tower::ServiceExt as _;
use tracing::subscriber::DefaultGuard;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry;

use super::running::Running;
use super::running::test_support::{create_test_running, create_test_running_with_operation};
use super::starting::{build_http_service, with_telemetry_layers};
use super::telemetry::{MCP_SESSION_ID, w3c_text_map_propagator};
use crate::health::{HealthCheck, HealthCheckConfig};

/// An inbound `traceparent` whose trace and span IDs the tests assert on.
const TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
const REMOTE_TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const REMOTE_SPAN_ID: &str = "00f067aa0ba902b7";

/// Collects every span exported on the current thread for the life of the value.
///
/// The subscriber is thread-local, so tests stay independent of each other.
struct ExportedSpans {
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    _subscriber: DefaultGuard,
}

impl ExportedSpans {
    fn capture() -> Self {
        opentelemetry::global::set_text_map_propagator(w3c_text_map_propagator());
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = registry().with(OpenTelemetryLayer::new(provider.tracer("test")));
        Self {
            exporter,
            provider,
            _subscriber: tracing::subscriber::set_default(subscriber),
        }
    }

    fn collect(self) -> Vec<SpanData> {
        self.provider.force_flush().expect("flush exported spans");
        self.exporter
            .get_finished_spans()
            .expect("read exported spans")
    }
}

/// The router `Starting::start` builds: the MCP transport behind the telemetry
/// layers, with the health check registered after them.
fn production_router(running: Running) -> Router {
    let service = build_http_service(running, true, &Default::default());
    let router = with_telemetry_layers(Router::new().nest_service("/mcp", service));
    HealthCheck::new(HealthCheckConfig::default()).enable_router(router)
}

fn mcp_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("Host", "localhost:8000")
        .header("User-Agent", "test-agent/1.0")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(Body::from(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "1.0.0"},
                },
            })
            .to_string(),
        ))
        .expect("valid test request")
}

/// Run one request through the production router and return every exported span.
async fn spans_for(request: Request<Body>) -> Vec<SpanData> {
    let spans = ExportedSpans::capture();
    production_router(create_test_running())
        .oneshot(request)
        .await
        .expect("router responds");
    spans.collect()
}

/// Run one request through the production router and return its `SERVER` span.
async fn server_span_for(request: Request<Body>) -> SpanData {
    let mut server_spans = spans_for(request)
        .await
        .into_iter()
        .filter(|span| span.span_kind == SpanKind::Server)
        .collect::<Vec<_>>();
    assert_eq!(
        server_spans.len(),
        1,
        "expected exactly one SERVER span, got {server_spans:#?}"
    );
    server_spans.remove(0)
}

fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a OtelValue> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| &attribute.value)
}

fn attribute_or_panic<'a>(span: &'a SpanData, key: &str) -> &'a OtelValue {
    attribute(span, key).unwrap_or_else(|| panic!("span should carry {key}: {span:#?}"))
}

mod server_span {
    use super::*;

    #[tokio::test]
    async fn is_the_only_server_span_exported_for_a_request() {
        let server_spans = spans_for(mcp_request("/mcp"))
            .await
            .into_iter()
            .filter(|span| span.span_kind == SpanKind::Server)
            .count();

        assert_eq!(server_spans, 1);
    }

    #[tokio::test]
    async fn is_named_after_the_method_and_route() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(span.name, "POST /mcp");
    }

    #[tokio::test]
    async fn is_a_trace_root() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(span.parent_span_id, opentelemetry::trace::SpanId::INVALID);
    }

    #[tokio::test]
    async fn records_the_request_method() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(
            attribute_or_panic(&span, "http.request.method").as_str(),
            "POST"
        );
    }

    #[tokio::test]
    async fn records_the_route() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(attribute_or_panic(&span, "http.route").as_str(), "/mcp");
    }

    #[tokio::test]
    async fn records_the_url_path() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(attribute_or_panic(&span, "url.path").as_str(), "/mcp");
    }

    #[tokio::test]
    async fn records_the_url_scheme() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(attribute_or_panic(&span, "url.scheme").as_str(), "http");
    }

    #[tokio::test]
    async fn records_the_server_address_without_the_port() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(
            attribute_or_panic(&span, "server.address").as_str(),
            "localhost"
        );
    }

    #[tokio::test]
    async fn records_the_server_port() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(
            attribute_or_panic(&span, "server.port"),
            &OtelValue::I64(8000)
        );
    }

    #[tokio::test]
    async fn records_the_user_agent() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(
            attribute_or_panic(&span, "user_agent.original").as_str(),
            "test-agent/1.0"
        );
    }

    #[tokio::test]
    async fn records_the_protocol_version() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(
            attribute_or_panic(&span, "network.protocol.version").as_str(),
            "1.1"
        );
    }

    #[tokio::test]
    async fn records_the_response_status_code_as_an_integer() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(
            attribute_or_panic(&span, "http.response.status_code"),
            &OtelValue::I64(200)
        );
    }

    #[tokio::test]
    async fn records_the_mcp_session_id() {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert!(
            !attribute_or_panic(&span, MCP_SESSION_ID)
                .as_str()
                .is_empty(),
            "session id should be recorded: {span:#?}"
        );
    }

    #[tokio::test]
    async fn records_the_session_id_the_client_sent() {
        // Only `initialize` gets a session back in the response; every later
        // request carries it inbound.
        let mut request = mcp_request("/mcp");
        request.headers_mut().insert(
            "mcp-session-id",
            "client-session".parse().expect("valid header value"),
        );

        let span = server_span_for(request).await;

        assert_eq!(
            attribute_or_panic(&span, MCP_SESSION_ID).as_str(),
            "client-session"
        );
    }

    #[rstest::rstest]
    // The names the 1.19 and earlier span used. Collector rules that match them
    // must move to the semantic-convention names above.
    #[case::method("method")]
    #[case::uri("uri")]
    #[case::status_code("status_code")]
    #[tokio::test]
    async fn no_longer_records_the_legacy_attribute(#[case] key: &str) {
        let span = server_span_for(mcp_request("/mcp")).await;

        assert_eq!(attribute(&span, key), None);
    }
}

mod span_status {
    use super::*;

    /// A route that fails, wrapped in the production telemetry layers. The MCP
    /// transport has no reliable way to return a 5xx on demand.
    async fn status_for(response_status: StatusCode) -> Status {
        span_for(response_status).await.status
    }

    async fn span_for(response_status: StatusCode) -> SpanData {
        let spans = ExportedSpans::capture();
        let router = with_telemetry_layers(Router::new().route(
            "/fail",
            axum::routing::get(move || async move { response_status }),
        ));
        router
            .oneshot(
                Request::builder()
                    .uri("/fail")
                    .body(Body::empty())
                    .expect("valid test request"),
            )
            .await
            .expect("router responds");

        let mut spans = spans
            .collect()
            .into_iter()
            .filter(|span| span.span_kind == SpanKind::Server)
            .collect::<Vec<_>>();
        assert_eq!(spans.len(), 1, "expected one SERVER span: {spans:#?}");
        spans.remove(0)
    }

    #[tokio::test]
    async fn is_unset_for_a_successful_response() {
        assert_eq!(status_for(StatusCode::OK).await, Status::Unset);
    }

    #[tokio::test]
    async fn is_unset_for_a_client_error() {
        assert_eq!(status_for(StatusCode::BAD_REQUEST).await, Status::Unset);
    }

    #[tokio::test]
    async fn is_error_for_a_server_error() {
        assert_eq!(
            status_for(StatusCode::INTERNAL_SERVER_ERROR).await,
            Status::error("")
        );
    }

    #[tokio::test]
    async fn records_the_error_type_for_a_server_error() {
        let span = span_for(StatusCode::INTERNAL_SERVER_ERROR).await;

        assert_eq!(attribute_or_panic(&span, "error.type").as_str(), "500");
    }

    #[tokio::test]
    async fn records_no_error_type_for_a_successful_response() {
        let span = span_for(StatusCode::OK).await;

        assert_eq!(attribute(&span, "error.type"), None);
    }
}

mod inbound_trace_context {
    use super::*;

    fn traced_request() -> Request<Body> {
        let mut request = mcp_request("/mcp");
        request.headers_mut().insert(
            "traceparent",
            TRACEPARENT.parse().expect("valid traceparent"),
        );
        request
    }

    #[tokio::test]
    async fn joins_the_inbound_trace() {
        let span = server_span_for(traced_request()).await;

        assert_eq!(span.span_context.trace_id().to_string(), REMOTE_TRACE_ID);
    }

    #[tokio::test]
    async fn parents_the_span_to_the_inbound_span() {
        let span = server_span_for(traced_request()).await;

        assert_eq!(span.parent_span_id.to_string(), REMOTE_SPAN_ID);
    }
}

mod sub_paths {
    use super::*;

    // axum reports no `MatchedPath` for a nested catch-all match, so subpaths
    // of the MCP endpoint are traced without a route. Only the exact endpoint
    // path gets a route-qualified span name.

    #[tokio::test]
    async fn are_named_after_the_method_alone() {
        let span = server_span_for(mcp_request("/mcp/anything")).await;

        assert_eq!(span.name, "POST");
    }

    #[tokio::test]
    async fn record_no_route() {
        let span = server_span_for(mcp_request("/mcp/anything")).await;

        assert_eq!(attribute(&span, "http.route"), None);
    }
}

mod health_check {
    use super::*;

    #[tokio::test]
    async fn is_not_traced() {
        // The health route is registered after the telemetry layers so probes
        // don't show up as service entry points.
        let spans = spans_for(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("valid test request"),
        )
        .await;

        assert!(spans.is_empty(), "health probe was traced: {spans:#?}");
    }
}

mod span_tree {
    use super::*;
    use http_body_util::BodyExt as _;

    /// Drive a tool call end to end so the whole tree is exported: the SERVER
    /// span, the MCP handler span, `execute`, and the outgoing GraphQL call.
    async fn spans_for_a_tool_call() -> Vec<SpanData> {
        let mut graphql = mockito::Server::new_async().await;
        let endpoint = graphql
            .mock("POST", "/")
            .with_body(r#"{"data": {"hello": "world"}}"#)
            .create_async()
            .await;

        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("Host", "localhost:8000")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Protocol-Version", "2025-11-25")
            .body(Body::from(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {"name": "Hello", "arguments": {}},
                })
                .to_string(),
            ))
            .expect("valid test request");

        let spans = ExportedSpans::capture();
        let running = create_test_running_with_operation(
            graphql.url().parse().expect("mock server URL is a URL"),
        );
        let service = build_http_service(running, false, &Default::default());
        let response = with_telemetry_layers(Router::new().nest_service("/mcp", service))
            .oneshot(request)
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        // The tool runs while the response body streams.
        response
            .into_body()
            .collect()
            .await
            .expect("read response body");
        endpoint.assert();

        spans.collect()
    }

    fn named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| span.name == name)
            .unwrap_or_else(|| {
                let names: Vec<_> = spans.iter().map(|span| &span.name).collect();
                panic!("no {name} span among {names:?}")
            })
    }

    #[tokio::test]
    async fn exports_one_server_span_for_the_whole_call() {
        let spans = spans_for_a_tool_call().await;

        let server_spans: Vec<_> = spans
            .iter()
            .filter(|span| span.span_kind == SpanKind::Server)
            .map(|span| &span.name)
            .collect();
        assert_eq!(server_spans, vec!["POST /mcp"]);
    }

    #[tokio::test]
    async fn parents_the_handler_span_to_the_server_span() {
        let spans = spans_for_a_tool_call().await;

        assert_eq!(
            named(&spans, "call_tool").parent_span_id,
            named(&spans, "POST /mcp").span_context.span_id()
        );
    }

    #[tokio::test]
    async fn parents_the_execute_span_to_the_handler_span() {
        let spans = spans_for_a_tool_call().await;

        assert_eq!(
            named(&spans, "execute").parent_span_id,
            named(&spans, "call_tool").span_context.span_id()
        );
    }

    #[tokio::test]
    async fn keeps_the_outgoing_graphql_span_a_client_span() {
        let spans = spans_for_a_tool_call().await;

        assert_eq!(
            named(&spans, "mcp-graphql-client").span_kind,
            SpanKind::Client
        );
    }

    #[rstest::rstest]
    #[case::handler("call_tool")]
    #[case::execution("execute")]
    #[case::graphql_client("mcp-graphql-client")]
    #[tokio::test]
    async fn keeps_the_request_path_in_the_server_span_trace(#[case] name: &str) {
        let spans = spans_for_a_tool_call().await;

        assert_eq!(
            named(&spans, name).span_context.trace_id(),
            named(&spans, "POST /mcp").span_context.trace_id()
        );
    }

    #[tokio::test]
    async fn ends_after_the_children_it_encloses() {
        // The transport returns the response head before the tool runs, so the
        // span has to stay open across the streaming body. Otherwise backends
        // read a near-zero request duration and a parent that closes first.
        let spans = spans_for_a_tool_call().await;

        assert!(
            named(&spans, "POST /mcp").end_time >= named(&spans, "call_tool").end_time,
            "SERVER span ended before its child: {spans:#?}"
        );
    }

    #[tokio::test]
    async fn leaves_tool_loading_an_internal_root() {
        // Startup work must stay off the request path: a collector rule that
        // promoted it to SERVER would count tool loads as inbound requests.
        let spans = spans_for_a_tool_call().await;

        let load_tool = named(&spans, "load_tool");
        assert_eq!(
            (load_tool.span_kind.clone(), load_tool.parent_span_id),
            (SpanKind::Internal, opentelemetry::trace::SpanId::INVALID)
        );
    }
}
