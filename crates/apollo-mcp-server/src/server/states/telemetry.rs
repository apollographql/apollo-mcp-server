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
