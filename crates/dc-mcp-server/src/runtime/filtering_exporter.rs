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

    fn shutdown(&mut self) -> OTelSdkResult {
        self.inner.shutdown()
    }
    fn force_flush(&mut self) -> OTelSdkResult {
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
    use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanExporter, SpanLinks};
    use std::collections::HashSet;
    use std::fmt::Debug;
    use std::future::ready;
    use std::time::SystemTime;

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn create_mock_span_data() -> SpanData {
        let span_context: SpanContext = SpanContext::new(
            TraceId::from_u128(1),
            SpanId::from_u64(12345),
            TraceFlags::default(),
            true, // is_remote
            TraceState::default(),
        );

        SpanData {
            span_context,
            parent_span_id: SpanId::from_u64(54321),
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
        #[derive(Debug)]
        struct TestExporter {}

        #[cfg_attr(coverage_nightly, coverage(off))]
        impl SpanExporter for TestExporter {
            fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
                batch.into_iter().for_each(|span| {
                    if span
                        .attributes
                        .iter()
                        .any(|kv| kv.key.as_str().starts_with("apollo."))
                    {
                        panic!("Omitted attributes were not filtered");
                    }
                });

                ready(Ok(()))
            }

            fn shutdown(&mut self) -> OTelSdkResult {
                Ok(())
            }

            fn force_flush(&mut self) -> OTelSdkResult {
                Ok(())
            }

            fn set_resource(&mut self, _resource: &Resource) {}
        }

        let mut omitted = HashSet::new();
        omitted.insert(Key::from_static_str("apollo.mock"));
        let mock_exporter = TestExporter {};
        let mock_span_data = create_mock_span_data();

        let filtering_exporter = FilteringExporter::new(mock_exporter, omitted);
        filtering_exporter
            .export(vec![mock_span_data])
            .await
            .expect("Export error");
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_shutdown() {
        #[derive(Debug)]
        struct TestExporter {}

        #[cfg_attr(coverage_nightly, coverage(off))]
        impl SpanExporter for TestExporter {
            fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
                ready(Err(OTelSdkError::InternalFailure(
                    "unexpected call".to_string(),
                )))
            }

            fn shutdown(&mut self) -> OTelSdkResult {
                Ok(())
            }

            fn force_flush(&mut self) -> OTelSdkResult {
                Err(OTelSdkError::InternalFailure("unexpected call".to_string()))
            }

            fn set_resource(&mut self, _resource: &Resource) {
                unreachable!("should not be called");
            }
        }

        let mock_exporter = TestExporter {};

        let mut filtering_exporter = FilteringExporter::new(mock_exporter, HashSet::new());
        assert!(filtering_exporter.shutdown().is_ok());
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_force_flush() {
        #[derive(Debug)]
        struct TestExporter {}

        #[cfg_attr(coverage_nightly, coverage(off))]
        impl SpanExporter for TestExporter {
            fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
                ready(Err(OTelSdkError::InternalFailure(
                    "unexpected call".to_string(),
                )))
            }

            fn shutdown(&mut self) -> OTelSdkResult {
                Err(OTelSdkError::InternalFailure("unexpected call".to_string()))
            }

            fn force_flush(&mut self) -> OTelSdkResult {
                Ok(())
            }

            fn set_resource(&mut self, _resource: &Resource) {
                unreachable!("should not be called");
            }
        }

        let mock_exporter = TestExporter {};

        let mut filtering_exporter = FilteringExporter::new(mock_exporter, HashSet::new());
        assert!(filtering_exporter.force_flush().is_ok());
    }

    #[tokio::test]
    async fn filtering_exporter_calls_inner_exporter_on_set_resource() {
        #[derive(Debug)]
        struct TestExporter {}

        #[cfg_attr(coverage_nightly, coverage(off))]
        impl SpanExporter for TestExporter {
            fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
                ready(Err(OTelSdkError::InternalFailure(
                    "unexpected call".to_string(),
                )))
            }

            fn shutdown(&mut self) -> OTelSdkResult {
                Err(OTelSdkError::InternalFailure("unexpected call".to_string()))
            }

            fn force_flush(&mut self) -> OTelSdkResult {
                Err(OTelSdkError::InternalFailure("unexpected call".to_string()))
            }

            fn set_resource(&mut self, _resource: &Resource) {}
        }

        let mock_exporter = TestExporter {};

        let mut filtering_exporter = FilteringExporter::new(mock_exporter, HashSet::new());
        filtering_exporter.set_resource(&Resource::builder_empty().build());
    }
}
