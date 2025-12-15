use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::global;
use opentelemetry::propagation::Extractor;
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

// Custom extractor for axum headers
struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

// Implement the Extractor trait for HeaderExtractor
impl<'a> Extractor for HeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Middleware that extracts and stores OpenTelemetry context in request extensions
pub async fn otel_context_middleware(mut request: Request, next: Next) -> Response {
    let parent_cx = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });

    let span = tracing::info_span!(
        "mcp_server",
        method = %request.method(),
        uri = %request.uri(),
        session_id = tracing::field::Empty,
        status_code = tracing::field::Empty,
    );
    span.set_parent(parent_cx);

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
        .unwrap_or_else(tracing::Span::current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request, routing::get};
    use opentelemetry::trace::{TraceContextExt, TracerProvider};
    use tower::ServiceExt;

    static INIT: std::sync::Once = std::sync::Once::new();

    // Initialize OpenTelemetry
    fn setup_telemetry() {
        INIT.call_once(|| {
            use opentelemetry_sdk::trace::SdkTracerProvider;
            use tracing_subscriber::layer::SubscriberExt;

            let tracer_provider = SdkTracerProvider::builder().build();
            let tracer = tracer_provider.tracer("test");

            // Set global propagator
            opentelemetry::global::set_text_map_propagator(
                opentelemetry_sdk::propagation::TraceContextPropagator::new(),
            );

            // Create subscriber with OpenTelemetry layer
            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
            let subscriber = tracing_subscriber::registry()
                .with(telemetry)
                .with(tracing_subscriber::fmt::layer());

            let _ = tracing::subscriber::set_global_default(subscriber);
        })
    }

    #[tokio::test]
    async fn test_middleware_stores_span_context_and_handler_works() {
        setup_telemetry();

        async fn test_handler(req: Request<Body>) -> &'static str {
            let (parts, _body) = req.into_parts();

            // Verify span was stored in extensions
            let stored_span = parts.extensions.get::<tracing::Span>();
            assert!(stored_span.is_some(), "Span should be stored in extensions");

            let span = stored_span.unwrap();
            assert_eq!(span.metadata().map(|m| m.name()), Some("mcp_server"));

            // Verify the span has the correct OpenTelemetry context with extracted trace_id
            let otel_context = span.context();
            let span_ref = otel_context.span();
            let span_context = span_ref.span_context();

            // The traceparent header contains trace_id: 4bf92f3577b34da6a3ce929d0e0e4736
            let expected_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
            let actual_trace_id = format!("{:032x}", span_context.trace_id());

            assert_eq!(
                actual_trace_id, expected_trace_id,
                "Trace ID should match the traceparent header"
            );

            // Verify trace is valid (not all zeros)
            assert!(span_context.is_valid(), "Span context should be valid");

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
    async fn test_middleware_works_without_traceparent() {
        setup_telemetry();

        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(otel_context_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), 200);
    }

    #[test]
    fn test_header_extractor_gets_values() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("traceparent", "test-value".parse().unwrap());
        headers.insert("x-custom", "custom-value".parse().unwrap());

        let extractor = HeaderExtractor(&headers);

        assert_eq!(extractor.get("traceparent"), Some("test-value"));
        assert_eq!(extractor.get("x-custom"), Some("custom-value"));
        assert_eq!(extractor.get("missing"), None);
    }
}
