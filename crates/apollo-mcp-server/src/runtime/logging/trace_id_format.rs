//! Custom event format that prepends `trace_id=<hex> span_id=<hex>` to log
//! lines when an OpenTelemetry trace context is active.

use std::fmt;

use opentelemetry::Context;
use opentelemetry::trace::{SpanId, TraceContextExt, TraceId};
use tracing::Subscriber;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{Format, FormatEvent, FormatFields, Full, Writer};
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::registry::LookupSpan;

/// A [`FormatEvent`] wrapper that prepends `trace_id=<hex> span_id=<hex>` when
/// the current span carries an OpenTelemetry trace context.
pub struct TraceIdFormat {
    inner: Format<Full, SystemTime>,
}

impl TraceIdFormat {
    /// Wraps `inner` with a `trace_id=<hex> span_id=<hex>` prefix derived from
    /// the active OpenTelemetry context, if any.
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
        if let Some((trace_id, span_id)) = current_ids() {
            write!(writer, "trace_id={trace_id} span_id={span_id} ")?;
        }

        self.inner.format_event(ctx, writer, event)
    }
}

/// Reads the trace and span IDs from the currently attached OpenTelemetry
/// context, or `None` if no span with a valid trace is active.
///
/// `tracing-opentelemetry`'s layer attaches an OTel context guard when a span
/// is entered (when `context_activation` is enabled, which is the default), so
/// during event formatting the current OTel context corresponds to the
/// innermost active span. Events with an explicit `parent:` still get the IDs
/// of that entered span, not of the named parent.
pub(super) fn current_ids() -> Option<(TraceId, SpanId)> {
    Context::map_current(|context| {
        let span = context.span();
        let span_context = span.span_context();
        span_context
            .is_valid()
            .then(|| (span_context.trace_id(), span_context.span_id()))
    })
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::fmt::format::Format;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry;

    use super::super::test_writer::TestWriter;
    use super::*;

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
    fn trace_and_span_ids_appear_when_span_active() {
        let writer = TestWriter::new();
        let subscriber = test_subscriber(writer.clone());

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("test_span");
            let _guard = span.enter();
            tracing::info!("hello from span");
        });

        let output = writer.contents();
        let re =
            regex::Regex::new(r"trace_id=[0-9a-f]{32} span_id=[0-9a-f]{16} ").expect("valid regex");
        assert!(
            re.is_match(&output),
            "expected trace_id=<32hex> span_id=<16hex> in output, got: {output}"
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
            !output.contains("trace_id=") && !output.contains("span_id="),
            "expected no trace_id or span_id in output, got: {output}"
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
    fn child_span_has_its_own_span_id() {
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
        let re = regex::Regex::new(r"span_id=([0-9a-f]{16})").expect("valid regex");
        let ids: Vec<String> = re
            .captures_iter(&output)
            .map(|c| c[1].to_string())
            .collect();

        assert_eq!(ids.len(), 2, "expected 2 span_id entries: {output}");
        assert_ne!(
            ids[0], ids[1],
            "parent and child should have different span_ids"
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
