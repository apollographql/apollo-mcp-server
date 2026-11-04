//! [`SpanExporter`] decorator that strips configured attributes from each
//! span before delegating to an inner exporter.
//!
//! Users opt attributes out via `telemetry.exporters.tracing.omitted_attributes`
//! in YAML; the keys land in [`FilteringExporter::new`] and are removed at
//! export time so the inner exporter (typically OTLP) never sees them.
//! Filtering applies only to this server's exported span attributes; log output
//! and telemetry emitted by downstream services use their own configuration.

use opentelemetry::Key;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use std::collections::HashSet;
use std::time::Duration;

const GEN_AI_TOOL_NAME: Key = Key::from_static_str("gen_ai.tool.name");
const MCP_METHOD_NAME: Key = Key::from_static_str("mcp.method.name");

/// Wraps an inner [`SpanExporter`] and drops any attribute whose key appears
/// in `omitted` before the span is exported.
///
/// All other attributes pass through untouched. Lifecycle methods
/// (`shutdown`, `shutdown_with_timeout`, `force_flush`, `set_resource`) are
/// forwarded transparently.
#[derive(Debug)]
pub struct FilteringExporter<E> {
    inner: E,
    omitted: HashSet<Key>,
}

impl<E> FilteringExporter<E> {
    /// Builds a [`FilteringExporter`] around `inner` that will strip every
    /// attribute in `omitted` from exported spans.
    pub fn new(inner: E, omitted: impl IntoIterator<Item = Key>) -> Self {
        Self {
            inner,
            omitted: omitted.into_iter().collect(),
        }
    }
}

impl<E> SpanExporter for FilteringExporter<E>
where
    E: SpanExporter + Send + Sync,
{
    fn export(&self, mut batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        let omits_tool_name = self.omitted.contains(&GEN_AI_TOOL_NAME);
        for span in &mut batch {
            if omits_tool_name {
                drop_tool_name_from_span_name(span);
            }
            span.attributes.retain(|kv| !self.omitted.contains(&kv.key));
        }

        self.inner.export(batch)
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, r: &Resource) {
        self.inner.set_resource(r)
    }
}

/// Names a span that carries a tool name, such as `tools/call GetUser`, after
/// its MCP method alone, so an omitted tool name doesn't leave in the name.
fn drop_tool_name_from_span_name(span: &mut SpanData) {
    let carries_tool_name = span.attributes.iter().any(|kv| kv.key == GEN_AI_TOOL_NAME);
    let method = span
        .attributes
        .iter()
        .find(|kv| kv.key == MCP_METHOD_NAME)
        .map(|kv| kv.value.as_str().into_owned());
    if carries_tool_name && let Some(method) = method {
        span.name = method.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{SpanContext, SpanKind, Status, TraceState};
    use opentelemetry::{InstrumentationScope, Key, KeyValue, SpanId, TraceFlags, TraceId};
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanExporter, SpanLinks};
    use std::collections::HashSet;
    use std::future::ready;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    /// Inner exporter used in tests: counts invocations of every lifecycle
    /// method, records the attribute keys it receives, and panics if a
    /// filtered `apollo.*` attribute leaks through `export`.
    #[derive(Debug, Default, Clone)]
    struct RecordingExporter {
        exports: Arc<AtomicUsize>,
        exported_keys: Arc<std::sync::Mutex<Vec<Key>>>,
        exported_names: Arc<std::sync::Mutex<Vec<String>>>,
        shutdowns: Arc<AtomicUsize>,
        shutdown_timeouts: Arc<std::sync::Mutex<Vec<Duration>>>,
        force_flushes: Arc<AtomicUsize>,
        set_resources: Arc<AtomicUsize>,
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    impl SpanExporter for RecordingExporter {
        fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
            for span in &batch {
                assert!(
                    !span
                        .attributes
                        .iter()
                        .any(|kv| kv.key.as_str().starts_with("apollo.")),
                    "apollo.* attribute leaked through the filter",
                );
                self.exported_keys
                    .lock()
                    .expect("lock poisoned")
                    .extend(span.attributes.iter().map(|kv| kv.key.clone()));
                self.exported_names
                    .lock()
                    .expect("lock poisoned")
                    .push(span.name.to_string());
            }
            self.exports.fetch_add(1, Ordering::SeqCst);
            ready(Ok(()))
        }

        fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
            self.shutdown_timeouts
                .lock()
                .expect("lock poisoned")
                .push(timeout);
            Ok(())
        }

        fn shutdown(&self) -> OTelSdkResult {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn force_flush(&self) -> OTelSdkResult {
            self.force_flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn set_resource(&mut self, _resource: &Resource) {
            self.set_resources.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn create_mock_span_data() -> SpanData {
        let span_context: SpanContext = SpanContext::new(
            TraceId::from_bytes(1u128.to_be_bytes()),
            SpanId::from_bytes(12345u64.to_be_bytes()),
            TraceFlags::default(),
            true, // is_remote
            TraceState::default(),
        );

        SpanData {
            span_context,
            parent_span_id: SpanId::from_bytes(54321u64.to_be_bytes()),
            parent_span_is_remote: false,
            span_kind: SpanKind::Internal,
            name: "test-span".into(),
            start_time: SystemTime::UNIX_EPOCH,
            end_time: SystemTime::UNIX_EPOCH,
            attributes: vec![
                KeyValue::new("http.method", "GET"),
                KeyValue::new("apollo.mock", "mock"),
            ],
            dropped_attributes_count: 0,
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status: Status::Ok,
            instrumentation_scope: InstrumentationScope::builder("test-service")
                .with_version("1.0.0")
                .build(),
        }
    }

    #[tokio::test]
    async fn filtering_exporter_filters_omitted_apollo_attributes() {
        let mut omitted = HashSet::new();
        omitted.insert(Key::from_static_str("apollo.mock"));
        let recorder = RecordingExporter::default();
        let exports = recorder.exports.clone();

        let filtering_exporter = FilteringExporter::new(recorder, omitted);
        assert!(
            filtering_exporter
                .export(vec![create_mock_span_data()])
                .await
                .is_ok()
        );

        assert_eq!(exports.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn filtering_exporter_filters_omitted_non_apollo_attributes() {
        let recorder = RecordingExporter::default();
        let exported_keys = recorder.exported_keys.clone();

        let filtering_exporter = FilteringExporter::new(
            recorder,
            [
                Key::from_static_str("apollo.mock"),
                Key::from_static_str("http.method"),
            ],
        );
        assert!(
            filtering_exporter
                .export(vec![create_mock_span_data()])
                .await
                .is_ok()
        );

        assert!(exported_keys.lock().expect("lock poisoned").is_empty());
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn tool_call_span() -> SpanData {
        SpanData {
            name: "tools/call GetUser".into(),
            attributes: vec![
                KeyValue::new("mcp.method.name", "tools/call"),
                KeyValue::new("gen_ai.tool.name", "GetUser"),
            ],
            ..create_mock_span_data()
        }
    }

    #[rstest::rstest]
    #[case::tool_name_omitted(&["apollo.mcp.tool_name", "gen_ai.tool.name"], "tools/call")]
    #[case::tool_name_kept(&["apollo.mock"], "tools/call GetUser")]
    #[tokio::test]
    async fn filtering_exporter_names_tool_call_spans(
        #[case] omitted: &[&'static str],
        #[case] expected: &str,
    ) {
        let recorder = RecordingExporter::default();
        let exported_names = recorder.exported_names.clone();

        let filtering_exporter =
            FilteringExporter::new(recorder, omitted.iter().map(|k| Key::from_static_str(k)));
        assert!(
            filtering_exporter
                .export(vec![tool_call_span()])
                .await
                .is_ok()
        );

        assert_eq!(
            *exported_names.lock().expect("lock poisoned"),
            vec![expected]
        );
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_shutdown() {
        let recorder = RecordingExporter::default();
        let shutdowns = recorder.shutdowns.clone();
        let force_flushes = recorder.force_flushes.clone();

        let filtering_exporter = FilteringExporter::new(recorder, HashSet::new());
        assert!(filtering_exporter.shutdown().is_ok());

        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(force_flushes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_shutdown_with_timeout() {
        let recorder = RecordingExporter::default();
        let timeouts = recorder.shutdown_timeouts.clone();
        let shutdowns = recorder.shutdowns.clone();

        let filtering_exporter = FilteringExporter::new(recorder, HashSet::new());
        let timeout = Duration::from_secs(3);
        assert!(filtering_exporter.shutdown_with_timeout(timeout).is_ok());

        assert_eq!(*timeouts.lock().expect("lock poisoned"), vec![timeout]);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_force_flush() {
        let recorder = RecordingExporter::default();
        let shutdowns = recorder.shutdowns.clone();
        let force_flushes = recorder.force_flushes.clone();

        let filtering_exporter = FilteringExporter::new(recorder, HashSet::new());
        assert!(filtering_exporter.force_flush().is_ok());

        assert_eq!(force_flushes.load(Ordering::SeqCst), 1);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_set_resource() {
        let recorder = RecordingExporter::default();
        let set_resources = recorder.set_resources.clone();

        let mut filtering_exporter = FilteringExporter::new(recorder, HashSet::new());
        filtering_exporter.set_resource(&Resource::builder_empty().build());

        assert_eq!(set_resources.load(Ordering::SeqCst), 1);
    }
}
