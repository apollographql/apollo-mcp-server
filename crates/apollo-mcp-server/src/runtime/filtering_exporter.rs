use opentelemetry::{Key, KeyValue};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use std::collections::HashSet;
use std::fmt::Debug;

#[derive(Debug)]
pub struct FilteringExporter<E> {
    inner: E,
    omitted: HashSet<Key>,
}

impl<E> FilteringExporter<E> {
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
        for span in &mut batch {
            span.attributes
                .retain(|kv| filter_omitted_apollo_attributes(kv, &self.omitted));
        }

        self.inner.export(batch)
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

fn filter_omitted_apollo_attributes(kv: &KeyValue, omitted_attributes: &HashSet<Key>) -> bool {
    !kv.key.as_str().starts_with("apollo.") || !omitted_attributes.contains(&kv.key)
}

#[cfg(test)]
mod tests {
    use crate::runtime::filtering_exporter::FilteringExporter;
    use opentelemetry::trace::{SpanContext, SpanKind, Status, TraceState};
    use opentelemetry::{InstrumentationScope, Key, KeyValue, SpanId, TraceFlags, TraceId};
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanExporter, SpanLinks};
    use std::collections::HashSet;
    use std::fmt::Debug;
    use std::future::ready;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    /// Inner exporter used in tests: counts invocations of every lifecycle
    /// method and panics if a filtered `apollo.*` attribute leaks through
    /// `export`.
    #[derive(Debug, Default, Clone)]
    struct RecordingExporter {
        exports: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
        force_flushes: Arc<AtomicUsize>,
        set_resources: Arc<AtomicUsize>,
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    impl SpanExporter for RecordingExporter {
        fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
            for span in &batch {
                if span
                    .attributes
                    .iter()
                    .any(|kv| kv.key.as_str().starts_with("apollo."))
                {
                    panic!("Omitted apollo.* attribute leaked through the filter");
                }
            }
            self.exports.fetch_add(1, Ordering::SeqCst);
            ready(Ok(()))
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
        filtering_exporter
            .export(vec![create_mock_span_data()])
            .await
            .expect("Export error");

        assert_eq!(exports.load(Ordering::SeqCst), 1);
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
