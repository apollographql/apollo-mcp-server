//! Custom event format that prepends `trace_id=<hex>` to log lines
//! when an OpenTelemetry trace context is active.

use std::fmt;

use opentelemetry::trace::TraceId;
use tracing::Subscriber;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{Format, FormatEvent, FormatFields, Full, Writer};
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::registry::LookupSpan;

/// A [`FormatEvent`] wrapper that prepends `trace_id=<hex>` when the current
/// span carries an OpenTelemetry trace context.
pub struct TraceIdFormat {
    inner: Format<Full, SystemTime>,
}

impl TraceIdFormat {
    pub fn new(inner: Format<Full, SystemTime>) -> Self {
        Self { inner }
    }
}

impl<S, N> FormatEvent<S, N> for TraceIdFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        if let Some(trace_id) = extract_trace_id(ctx) {
            write!(writer, "trace_id={trace_id} ")?;
        }

        self.inner.format_event(ctx, writer, event)
    }
}

/// Walk the span ancestry looking for the first `OtelData` with a valid trace ID.
fn extract_trace_id<S, N>(ctx: &FmtContext<'_, S, N>) -> Option<TraceId>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    ctx.event_scope()?.find_map(|span_ref| {
        let extensions = span_ref.extensions();
        let trace_id = extensions.get::<OtelData>()?.trace_id()?;
        (trace_id != TraceId::INVALID).then_some(trace_id)
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::fmt::format::Format;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry;

    use super::*;

    /// A thread-safe in-memory writer for capturing log output.
    #[derive(Clone)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl TestWriter {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn contents(&self) -> String {
            let buf = self.0.lock().expect("lock poisoned");
            String::from_utf8_lossy(&buf).to_string()
        }
    }

    impl io::Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("lock poisoned").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for TestWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Build a subscriber with our `TraceIdFormat` that writes to `writer`.
    fn test_subscriber(
        writer: TestWriter,
    ) -> impl Subscriber + for<'a> LookupSpan<'a> + Send + Sync {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");

        let format = Format::default().with_ansi(false).with_target(false);

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .event_format(TraceIdFormat::new(format));

        registry()
            .with(fmt_layer)
            .with(OpenTelemetryLayer::new(tracer))
    }

    #[test]
    fn trace_id_appears_when_span_active() {
        let writer = TestWriter::new();
        let subscriber = test_subscriber(writer.clone());

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("test_span");
            let _guard = span.enter();
            tracing::info!("hello from span");
        });

        let output = writer.contents();
        let re = regex::Regex::new(r"trace_id=[0-9a-f]{32} ").expect("valid regex");
        assert!(
            re.is_match(&output),
            "expected trace_id=<32hex> in output, got: {output}"
        );
        assert!(
            output.contains("hello from span"),
            "expected message in output, got: {output}"
        );
    }

    #[test]
    fn no_trace_id_outside_span() {
        let writer = TestWriter::new();
        let subscriber = test_subscriber(writer.clone());

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("no span here");
        });

        let output = writer.contents();
        assert!(
            !output.contains("trace_id="),
            "expected no trace_id in output, got: {output}"
        );
        assert!(
            output.contains("no span here"),
            "expected message in output, got: {output}"
        );
    }

    #[test]
    fn child_span_inherits_parent_trace_id() {
        let writer = TestWriter::new();
        let subscriber = test_subscriber(writer.clone());

        tracing::subscriber::with_default(subscriber, || {
            let parent = tracing::info_span!("parent");
            let _parent_guard = parent.enter();
            tracing::info!("from parent");

            let child = tracing::info_span!("child");
            let _child_guard = child.enter();
            tracing::info!("from child");
        });

        let output = writer.contents();
        let re = regex::Regex::new(r"trace_id=([0-9a-f]{32})").expect("valid regex");
        let ids: Vec<String> = re
            .captures_iter(&output)
            .map(|c| c[1].to_string())
            .collect();

        assert_eq!(
            ids.len(),
            2,
            "expected 2 trace_id entries, got {}: {output}",
            ids.len()
        );
        assert_eq!(
            ids[0], ids[1],
            "parent and child should share the same trace_id"
        );
    }

    #[test]
    fn inner_format_still_works() {
        let writer = TestWriter::new();
        let subscriber = test_subscriber(writer.clone());

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("fmt_test");
            let _guard = span.enter();
            tracing::info!("check format");
        });

        let output = writer.contents();
        assert!(
            output.contains("INFO"),
            "expected log level in output, got: {output}"
        );
        assert!(
            output.contains("check format"),
            "expected message in output, got: {output}"
        );
    }
}
