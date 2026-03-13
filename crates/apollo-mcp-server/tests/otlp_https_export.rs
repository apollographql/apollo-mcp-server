//! Verifies that the OTLP HTTP exporter can attempt TLS connections to
//! https:// endpoints.
//!
//! Before the fix, the default reqwest 0.12 client pulled in by
//! opentelemetry-otlp had no TLS features (due to Cargo feature unification
//! loss when the workspace moved to reqwest 0.13), causing an immediate
//! "invalid URL, scheme is not http" error instead of a connection-level error.
//!
//! The opentelemetry-otlp default (`reqwest-blocking-client`) creates an
//! internal tokio runtime inside the reqwest blocking client. That runtime
//! cannot be created or dropped inside another tokio `block_on` context, so
//! we run the entire export on a standalone OS thread and use
//! `futures::executor::block_on` (not tokio) to poll the async export.

use opentelemetry::trace::{SpanContext, SpanKind, Status, TraceState};
use opentelemetry::{InstrumentationScope, SpanId, TraceFlags, TraceId};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanExporter, SpanLinks};
use std::time::SystemTime;

#[test]
fn http_protobuf_exporter_supports_https_endpoints() {
    // Run everything on a dedicated thread so there is no ambient tokio
    // runtime context. The reqwest-blocking-client inside opentelemetry-otlp
    // creates its own runtime and panics if one already exists.
    let err_msg = std::thread::spawn(|| {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint("https://localhost:4318")
            .build()
            .expect("exporter should construct successfully");

        let span_data = SpanData {
            span_context: SpanContext::new(
                TraceId::from_bytes(1u128.to_be_bytes()),
                SpanId::from_bytes(1u64.to_be_bytes()),
                TraceFlags::default(),
                false,
                TraceState::default(),
            ),
            parent_span_id: SpanId::INVALID,
            parent_span_is_remote: false,
            span_kind: SpanKind::Internal,
            name: "test-span".into(),
            start_time: SystemTime::now(),
            end_time: SystemTime::now(),
            attributes: vec![],
            dropped_attributes_count: 0,
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status: Status::Ok,
            instrumentation_scope: InstrumentationScope::builder("test").build(),
        };

        // Export the span. Nothing is listening, so this will fail — but
        // the error type tells us whether TLS is configured.
        //
        // Use futures::executor instead of tokio::block_on because the
        // reqwest-blocking-client inside otel creates its own tokio runtime
        // and panics if nested inside another one.
        let result = futures::executor::block_on(exporter.export(vec![span_data]));
        let err = result.expect_err("export to non-listening endpoint should fail");
        format!("{err:?}")
    })
    .join()
    .expect("test thread panicked");

    assert!(
        !err_msg.contains("scheme is not http"),
        "TLS connector is missing — the default reqwest client used by \
         opentelemetry-otlp has no TLS support. This is caused by Cargo \
         feature unification loss when reqwest 0.12 (used by otel) and \
         reqwest 0.13 (workspace) coexist. Add the `reqwest-rustls` feature \
         to opentelemetry-otlp to fix. Error was: {err_msg}"
    );
}
