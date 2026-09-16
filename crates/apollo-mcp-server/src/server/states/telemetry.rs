use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum_tracing_opentelemetry::tracing_opentelemetry_instrumentation_sdk::http::{
    http_flavor, http_host, user_agent,
};
use http::HeaderValue;
use http::uri::Authority;
use http_body::{Body as HttpBody, Frame, SizeHint};
use opentelemetry::Context as OtelContext;
use opentelemetry::baggage::{BaggageExt, KeyValueMetadata};
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, TextMapCompositePropagator};
use opentelemetry::trace::{SpanKind, TraceContextExt, TraceId};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, HTTP_RESPONSE_STATUS_CODE, HTTP_ROUTE, OTEL_STATUS_CODE, SERVER_PORT,
};
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

struct HeaderExtractor<'a> {
    headers: &'a axum::http::HeaderMap,
    baggage: Option<String>,
}

impl<'a> HeaderExtractor<'a> {
    fn new(headers: &'a axum::http::HeaderMap) -> Self {
        Self {
            headers,
            baggage: combined_normalized_baggage(headers),
        }
    }
}

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case("baggage") {
            return self.baggage.as_deref();
        }
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.headers.keys().map(|k| k.as_str()).collect()
    }

    fn get_all(&self, key: &str) -> Option<Vec<&str>> {
        if key.eq_ignore_ascii_case("baggage") {
            return self.baggage.as_deref().map(|value| vec![value]);
        }
        self.get(key).map(|value| vec![value])
    }
}

/// Join inbound `baggage` fields in header order and percent-encode extra raw
/// `=` characters in each member's value so the SDK parser keeps them.
fn combined_normalized_baggage(headers: &axum::http::HeaderMap) -> Option<String> {
    let mut joined = String::new();
    for value in headers.get_all("baggage") {
        let Some(value) = value.to_str().ok() else {
            continue;
        };
        if !joined.is_empty() {
            joined.push(',');
        }
        joined.push_str(value);
    }
    if joined.is_empty() {
        None
    } else {
        Some(normalize_baggage_list(&joined))
    }
}

fn normalize_baggage_list(header: &str) -> String {
    header
        .split(',')
        .map(normalize_baggage_member)
        .collect::<Vec<_>>()
        .join(",")
}

fn normalize_baggage_member(member: &str) -> String {
    let (kv, metadata) = match member.split_once(';') {
        Some((kv, metadata)) => (kv, Some(metadata)),
        None => (member, None),
    };
    let Some((key, value)) = kv.split_once('=') else {
        return member.to_string();
    };
    let encoded_value = value.replace('=', "%3D");
    match metadata {
        Some(metadata) => format!("{key}={encoded_value};{metadata}"),
        None => format!("{key}={encoded_value}"),
    }
}

/// Drop baggage members whose decoded metadata cannot be re-injected as an HTTP header.
fn sanitize_baggage_for_http_injection(cx: OtelContext) -> OtelContext {
    let original_len = cx.baggage().len();
    if original_len == 0 {
        return cx;
    }

    let safe: Vec<KeyValueMetadata> = cx
        .baggage()
        .iter()
        .filter(|(_, (_, metadata))| metadata_can_form_http_header(metadata.as_str()))
        .map(|(key, (value, metadata))| {
            KeyValueMetadata::new(key.clone(), value.clone(), metadata.clone())
        })
        .collect();

    if safe.len() == original_len {
        cx
    } else {
        cx.with_baggage(safe)
    }
}

fn metadata_can_form_http_header(metadata: &str) -> bool {
    let metadata = metadata.trim();
    metadata.is_empty() || HeaderValue::from_str(metadata).is_ok()
}

/// Composite propagator for W3C Trace Context and W3C Baggage.
pub fn w3c_text_map_propagator() -> TextMapCompositePropagator {
    TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ])
}

/// Span attribute holding the MCP session id the response assigned.
///
/// There is no semantic convention for it, so it follows the `apollo.mcp.*`
/// naming the server's other span attributes use.
pub const MCP_SESSION_ID: &str = "apollo.mcp.session_id";

/// Open the OpenTelemetry `SERVER` span that every inbound request runs in,
/// and record the response on it once the request completes.
pub async fn otel_context_middleware(mut request: Request, next: Next) -> Response {
    let parent_cx = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor::new(request.headers()))
    });
    let parent_cx = sanitize_baggage_for_http_injection(parent_cx);

    request.extensions_mut().insert(parent_cx.clone()); // Store the OtelContext directly in extensions

    // Only the `initialize` response assigns a session; every later request
    // carries it inbound instead, so read both ends to label the whole session.
    let inbound_session_id = session_id(request.headers()).map(str::to_owned);

    let span = server_span(&request);
    let _ = span.set_parent(parent_cx);

    request.extensions_mut().insert(span.clone()); // Store the span in request extensions

    let response = next.run(request).instrument(span.clone()).await;

    let status = response.status();
    span.record(HTTP_RESPONSE_STATUS_CODE, i64::from(status.as_u16()));
    if status.is_server_error() {
        // The conventions leave a server span's status unset below 5xx, and ask
        // for the status code as `error.type` when the status signals the error.
        span.record(OTEL_STATUS_CODE, "ERROR");
        span.record(ERROR_TYPE, status.as_str());
    }

    if let Some(session_id) = session_id(response.headers()).or(inbound_session_id.as_deref()) {
        span.record(MCP_SESSION_ID, session_id);
    }

    response.map(|inner| Body::new(BodyWithSpan { inner, span }))
}

fn session_id(headers: &http::HeaderMap) -> Option<&str> {
    headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
}

/// Polls the response body inside the request span.
///
/// A streamable-HTTP POST returns its head as soon as the transport accepts the
/// request, then runs the tool while the SSE body streams. `tracing` ends a span
/// at its last exit, so a span left behind with the head would report a fraction
/// of the request's duration and close before its own children.
struct BodyWithSpan {
    inner: Body,
    span: tracing::Span,
}

impl HttpBody for BodyWithSpan {
    type Data = <Body as HttpBody>::Data;
    type Error = <Body as HttpBody>::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let _entered = this.span.enter();
        Pin::new(&mut this.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Build the span for an inbound request, following the [HTTP server span
/// conventions].
///
/// [HTTP server span conventions]: https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-server-span
fn server_span(request: &Request) -> tracing::Span {
    // axum reports no `MatchedPath` for a nested catch-all match, so a subpath
    // of the MCP endpoint has no route. The conventions want `http.route` left
    // off entirely in that case, not set to an empty string.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    let method = request.method();
    let name = match route {
        Some(route) => format!("{method} {route}"),
        None => method.to_string(),
    };
    // The conventions keep the host and the port in separate attributes, but
    // the `Host` header carries both.
    let host = http_host(request);
    let authority = host.parse::<Authority>().ok();
    let (address, port) = match &authority {
        Some(authority) => (authority.host(), authority.port_u16()),
        None => (host, None),
    };

    // `url.query` is deliberately absent: it is opt-in, nothing in the MCP
    // protocol uses it, and a client could put credentials there.
    let span = tracing::info_span!(
        "http_request",
        otel.name = name.as_str(),
        otel.kind = ?SpanKind::Server,
        otel.status_code = tracing::field::Empty,
        http.request.method = %method,
        http.route = tracing::field::Empty,
        http.response.status_code = tracing::field::Empty,
        // Requests arrive in origin form, so the URI carries no scheme of its
        // own. This server only ever serves plaintext HTTP.
        url.scheme = request.uri().scheme_str().unwrap_or("http"),
        url.path = request.uri().path(),
        network.protocol.version = %http_flavor(request.version()),
        server.address = address,
        server.port = tracing::field::Empty,
        user_agent.original = user_agent(request),
        error.type = tracing::field::Empty,
        apollo.mcp.session_id = tracing::field::Empty,
    );
    if let Some(route) = route {
        span.record(HTTP_ROUTE, route);
    }
    if let Some(port) = port {
        span.record(SERVER_PORT, i64::from(port));
    }
    span
}

// Helper function to retrieve the parent span from the request context
pub fn get_parent_span(context: &RequestContext<RoleServer>) -> tracing::Span {
    context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<tracing::Span>())
        .cloned()
        .unwrap_or_else(tracing::Span::none)
}

/// Returns the current OpenTelemetry trace ID as a lowercase 32-character
/// hex string, or an empty string when no trace context is active.
///
/// The format matches the `trace_id=<hex>` prefix emitted by the logging
/// layer, so callers (including Rhai scripts) can correlate the values they
/// emit with the rest of the server's output.
pub fn current_trace_id() -> String {
    let trace_id = tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id();
    if trace_id == TraceId::INVALID {
        String::new()
    } else {
        trace_id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request, routing::get};
    use http::HeaderName;
    use opentelemetry::Context as OtelContext;
    use opentelemetry::baggage::BaggageExt;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tower::ServiceExt;
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry;

    #[tokio::test()]
    async fn middleware_stores_span_context_and_handler_works() {
        opentelemetry::global::set_text_map_propagator(w3c_text_map_propagator());

        async fn test_handler(req: Request<Body>) -> &'static str {
            let (parts, _body) = req.into_parts();

            // Get OtelContext from extensions
            let otel_ctx = parts
                .extensions
                .get::<OtelContext>()
                .expect("OtelContext should be in extensions");

            let trace_id = format!("{:032x}", otel_ctx.span().span_context().trace_id());
            assert_eq!(trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");

            // Verify span is also stored
            let span = parts.extensions.get::<tracing::Span>();
            assert!(span.is_some());

            "ok"
        }

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(axum::middleware::from_fn(otel_context_middleware));

        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let request = Request::builder()
            .uri("/test")
            .header("traceparent", traceparent)
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn middleware_works_without_traceparent() {
        opentelemetry::global::set_text_map_propagator(w3c_text_map_propagator());

        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(otel_context_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn middleware_extracts_w3c_baggage_into_otel_context() {
        opentelemetry::global::set_text_map_propagator(w3c_text_map_propagator());

        async fn test_handler(req: Request<Body>) -> &'static str {
            let (parts, _body) = req.into_parts();

            let otel_ctx = parts
                .extensions
                .get::<OtelContext>()
                .expect("OtelContext should be in extensions");

            let baggage = otel_ctx.baggage();
            assert_eq!(baggage.get("userId").map(|v| v.as_str()), Some("alice"));
            assert_eq!(baggage.get("serverNode").map(|v| v.as_str()), Some("DF28"));

            let trace_id = format!("{:032x}", otel_ctx.span().span_context().trace_id());
            assert_eq!(trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");

            "ok"
        }

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(axum::middleware::from_fn(otel_context_middleware));

        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let request = Request::builder()
            .uri("/test")
            .header("traceparent", traceparent)
            .header("baggage", "userId=alice,serverNode=DF28")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }

    #[test]
    fn w3c_propagator_injects_and_extracts_baggage() {
        use opentelemetry::KeyValue;
        use opentelemetry::propagation::TextMapPropagator;
        use std::collections::HashMap;

        let propagator = w3c_text_map_propagator();
        let cx = OtelContext::current().with_baggage(vec![KeyValue::new("userId", "alice")]);

        let mut headers = HashMap::new();
        propagator.inject_context(&cx, &mut headers);

        let baggage_header = headers
            .get("baggage")
            .expect("composite propagator should inject a baggage header");
        assert!(baggage_header.contains("userId=alice"));

        let extracted = propagator.extract(&headers);
        assert_eq!(
            extracted.baggage().get("userId").map(|v| v.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn header_extractor_gets_values() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("traceparent", "test-value".parse().unwrap());
        headers.insert("x-custom", "custom-value".parse().unwrap());

        let extractor = HeaderExtractor::new(&headers);

        assert_eq!(extractor.get("traceparent"), Some("test-value"));
        assert_eq!(extractor.get("x-custom"), Some("custom-value"));
        assert_eq!(extractor.get("missing"), None);
    }

    #[test]
    fn current_trace_id_is_empty_when_no_active_span() {
        assert_eq!(current_trace_id(), "");
    }

    #[test]
    fn current_trace_id_returns_hex_when_span_has_otel_data() {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");
        let subscriber = registry().with(OpenTelemetryLayer::new(tracer));

        let captured = tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("test_span");
            let _guard = span.enter();
            current_trace_id()
        });

        let re = regex::Regex::new(r"^[0-9a-f]{32}$").expect("valid regex");
        assert!(
            re.is_match(&captured),
            "expected 32-hex trace_id, got: {captured}"
        );
    }

    #[test]
    fn header_extractor_keys() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("traceparent", "test-value".parse().unwrap());
        headers.insert("x-custom", "custom-value".parse().unwrap());

        let extractor = HeaderExtractor::new(&headers);

        let mut keys = extractor
            .keys()
            .into_iter()
            .map(|k| HeaderName::from_bytes(k.as_bytes()).unwrap())
            .collect::<Vec<_>>();

        let mut expected = vec![
            HeaderName::from_static("traceparent"),
            HeaderName::from_static("x-custom"),
        ];

        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        assert_eq!(keys, expected);
    }

    #[test]
    fn header_extractor_preserves_equals_in_baggage_values() {
        use opentelemetry::propagation::TextMapPropagator;
        use std::collections::HashMap;

        let mut headers = axum::http::HeaderMap::new();
        headers.insert("baggage", "userId=ali=ce".parse().unwrap());

        let extractor = HeaderExtractor::new(&headers);
        assert_eq!(extractor.get("baggage"), Some("userId=ali%3Dce"));
        assert_eq!(extractor.get_all("baggage"), Some(vec!["userId=ali%3Dce"]));

        let extracted = w3c_text_map_propagator().extract(&extractor);
        assert_eq!(
            extracted.baggage().get("userId").map(|v| v.as_str()),
            Some("ali=ce")
        );

        let mut outgoing = HashMap::new();
        w3c_text_map_propagator().inject_context(&extracted, &mut outgoing);
        assert_eq!(
            outgoing.get("baggage").map(String::as_str),
            Some("userId=ali%3Dce")
        );
    }

    #[test]
    fn header_extractor_leaves_baggage_metadata_equals_unchanged() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("baggage", "userId=alice;property=val=ue".parse().unwrap());

        let extractor = HeaderExtractor::new(&headers);
        assert_eq!(
            extractor.get("baggage"),
            Some("userId=alice;property=val=ue")
        );
    }

    #[test]
    fn header_extractor_preserves_malformed_baggage_members() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("baggage", "not-a-member,userId=alice".parse().unwrap());

        let extractor = HeaderExtractor::new(&headers);
        assert_eq!(extractor.get("baggage"), Some("not-a-member,userId=alice"));
    }

    #[test]
    fn header_extractor_joins_appended_baggage_fields() {
        use opentelemetry::propagation::TextMapPropagator;

        let mut headers = axum::http::HeaderMap::new();
        headers.append("baggage", "userId=alice".parse().unwrap());
        headers.append("baggage", "serverNode=DF28".parse().unwrap());

        let extractor = HeaderExtractor::new(&headers);
        assert_eq!(
            extractor.get("baggage"),
            Some("userId=alice,serverNode=DF28")
        );

        let extracted = w3c_text_map_propagator().extract(&extractor);
        assert_eq!(
            extracted.baggage().get("userId").map(|v| v.as_str()),
            Some("alice")
        );
        assert_eq!(
            extracted.baggage().get("serverNode").map(|v| v.as_str()),
            Some("DF28")
        );
    }

    #[test]
    fn sanitize_baggage_for_http_injection_drops_invalid_metadata() {
        use opentelemetry::baggage::KeyValueMetadata;
        use opentelemetry::propagation::TextMapPropagator;
        use std::collections::HashMap;

        let cx = OtelContext::current().with_baggage(vec![
            KeyValueMetadata::new("safe", "ok", ""),
            KeyValueMetadata::new("userId", "alice", "property=one\ntwo"),
        ]);

        let sanitized = sanitize_baggage_for_http_injection(cx);
        assert_eq!(
            sanitized.baggage().get("safe").map(|v| v.as_str()),
            Some("ok")
        );
        assert!(sanitized.baggage().get("userId").is_none());

        let mut outgoing = HashMap::new();
        w3c_text_map_propagator().inject_context(&sanitized, &mut outgoing);
        let baggage = outgoing
            .get("baggage")
            .expect("safe baggage should still be injected");
        assert_eq!(baggage, "safe=ok");
        assert!(HeaderValue::from_str(baggage).is_ok());
    }

    #[tokio::test]
    async fn middleware_extracts_baggage_from_multiple_header_fields() {
        opentelemetry::global::set_text_map_propagator(w3c_text_map_propagator());

        async fn test_handler(req: Request<Body>) -> &'static str {
            let (parts, _body) = req.into_parts();

            let otel_ctx = parts
                .extensions
                .get::<OtelContext>()
                .expect("OtelContext should be in extensions");

            let baggage = otel_ctx.baggage();
            assert_eq!(baggage.get("userId").map(|v| v.as_str()), Some("alice"));
            assert_eq!(baggage.get("serverNode").map(|v| v.as_str()), Some("DF28"));

            "ok"
        }

        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(axum::middleware::from_fn(otel_context_middleware));

        let mut request = Request::builder().uri("/test").body(Body::empty()).unwrap();
        request
            .headers_mut()
            .append("baggage", "userId=alice".parse().unwrap());
        request
            .headers_mut()
            .append("baggage", "serverNode=DF28".parse().unwrap());

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }
}
