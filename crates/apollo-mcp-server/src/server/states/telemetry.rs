use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::HeaderValue;
use opentelemetry::Context as OtelContext;
use opentelemetry::baggage::{BaggageExt, KeyValueMetadata};
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, TextMapCompositePropagator};
use opentelemetry::trace::{TraceContextExt, TraceId};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
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

// Middleware that extracts and stores OpenTelemetry context in request extensions
pub async fn otel_context_middleware(mut request: Request, next: Next) -> Response {
    let parent_cx = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor::new(request.headers()))
    });
    let parent_cx = sanitize_baggage_for_http_injection(parent_cx);

    request.extensions_mut().insert(parent_cx.clone()); // Store the OtelContext directly in extensions

    let span = tracing::info_span!(
        "mcp_server",
        method = %request.method(),
        uri = %request.uri(),
        session_id = tracing::field::Empty,
        status_code = tracing::field::Empty,
    );
    let _ = span.set_parent(parent_cx);

    request.extensions_mut().insert(span.clone()); // Store the span in request extensions

    let response = next.run(request).instrument(span.clone()).await;

    span.record("status_code", tracing::field::display(response.status()));

    if let Some(session_id) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        span.record("session_id", tracing::field::display(session_id));
    }

    response
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
