//! Custom event format that writes each log event as one JSON object per line,
//! with `trace_id` and `span_id` as top-level keys when an OpenTelemetry trace
//! context is active.

use std::fmt;

use serde_json::{Map, Value};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_log::NormalizeEvent;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::registry::LookupSpan;

use super::trace_id_format::current_ids;

/// Event field names that never become keys: `trace_id`, `span_id`, and `span`
/// only come from the active span, and `log.*` fields are metadata that
/// `tracing-log` adds to events bridged from the `log` crate.
fn is_reserved_field(name: &str) -> bool {
    matches!(name, "trace_id" | "span_id" | "span") || name.starts_with("log.")
}

/// A [`FormatEvent`] that writes each event as a single-line JSON object.
///
/// Event fields, including `message` when the event has one, become top-level
/// keys. The `timestamp`, `level`, and `target` keys take precedence over event
/// fields with the same name, and `trace_id`, `span_id`, and `span` (the current
/// span's name) only appear inside a span. The output never contains ANSI
/// escape codes.
pub struct JsonFormat;

impl<S, N> FormatEvent<S, N> for JsonFormat
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
        let mut object = Map::new();
        let mut timestamp = String::new();
        SystemTime.format_time(&mut Writer::new(&mut timestamp))?;
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        object.insert("timestamp".to_string(), timestamp.into());
        object.insert("level".to_string(), metadata.level().as_str().into());
        object.insert("target".to_string(), metadata.target().into());
        if let Some(span) = ctx.lookup_current() {
            object.insert("span".to_string(), span.name().into());
        }
        if let Some((trace_id, span_id)) = current_ids() {
            object.insert("trace_id".to_string(), trace_id.to_string().into());
            object.insert("span_id".to_string(), span_id.to_string().into());
        }
        event.record(&mut JsonVisitor(&mut object));

        let line = serde_json::to_string(&object).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

/// Records event fields into a JSON object, keeping numbers and booleans typed.
/// Keys already in the object and reserved fields are left out.
struct JsonVisitor<'a>(&'a mut Map<String, Value>);

impl JsonVisitor<'_> {
    fn insert(&mut self, field: &Field, value: Value) {
        if !is_reserved_field(field.name()) {
            self.0.entry(field.name()).or_insert(value);
        }
    }
}

impl Visit for JsonVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, format!("{value:?}").into());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value.into());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.into());
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use serde_json::json;
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry;

    use super::super::test_writer::TestWriter;
    use super::*;

    fn test_subscriber(
        writer: TestWriter,
    ) -> impl Subscriber + for<'a> LookupSpan<'a> + Send + Sync {
        let provider = SdkTracerProvider::builder().build();
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .event_format(JsonFormat);
        registry()
            .with(fmt_layer)
            .with(OpenTelemetryLayer::new(provider.tracer("test")))
    }

    /// Runs `log` under a JSON subscriber and parses every output line.
    fn json_lines(log: impl FnOnce()) -> Vec<Map<String, Value>> {
        let writer = TestWriter::new();
        tracing::subscriber::with_default(test_subscriber(writer.clone()), log);
        writer
            .contents()
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is a JSON object"))
            .collect()
    }

    #[test]
    fn writes_one_object_per_event() {
        let lines = json_lines(|| {
            tracing::info!("first");
            tracing::warn!("second");
        });
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn includes_message_level_and_target() {
        let line = json_lines(|| tracing::warn!("hello")).remove(0);
        assert_eq!(
            (
                &line["message"],
                &line["level"],
                line.contains_key("target")
            ),
            (&json!("hello"), &json!("WARN"), true)
        );
    }

    #[test]
    fn includes_timestamp() {
        let line = json_lines(|| tracing::info!("hello")).remove(0);
        assert!(line["timestamp"].as_str().is_some_and(|ts| !ts.is_empty()));
    }

    #[test]
    fn keeps_event_field_types() {
        let line =
            json_lines(|| tracing::info!(count = 3, ok = true, name = "x", "hello")).remove(0);
        assert_eq!(
            (&line["count"], &line["ok"], &line["name"]),
            (&json!(3), &json!(true), &json!("x"))
        );
    }

    #[test]
    fn reserved_keys_take_precedence_over_event_fields() {
        let line = json_lines(|| tracing::info!(level = "custom", "hello")).remove(0);
        assert_eq!(line["level"], json!("INFO"));
    }

    #[test]
    fn includes_trace_and_span_ids_in_span() {
        let line = json_lines(|| {
            let span = tracing::info_span!("test_span");
            let _guard = span.enter();
            tracing::info!("in span");
        })
        .remove(0);
        let hex_len = |key: &str| line[key].as_str().map(str::len);
        assert_eq!(
            (hex_len("trace_id"), hex_len("span_id")),
            (Some(32), Some(16))
        );
    }

    #[test]
    fn includes_current_span_name() {
        let line = json_lines(|| {
            let span = tracing::info_span!("call_tool");
            let _guard = span.enter();
            tracing::info!("in span");
        })
        .remove(0);
        assert_eq!(line["span"], json!("call_tool"));
    }

    #[test]
    fn ignores_id_fields_on_events_outside_span() {
        let line =
            json_lines(|| tracing::info!(trace_id = "fake", span_id = "fake", "hello")).remove(0);
        assert!(!line.contains_key("trace_id") && !line.contains_key("span_id"));
    }

    #[test]
    fn uses_original_target_for_log_crate_events() {
        let line = json_lines(|| {
            let record = log::Record::builder()
                .args(format_args!("from log"))
                .level(log::Level::Info)
                .target("some_dependency::module")
                .module_path_static(Some("some_dependency::module"))
                .file_static(Some("module.rs"))
                .line(Some(7))
                .build();
            tracing_log::format_trace(&record).expect("bridged log record");
        })
        .remove(0);
        assert_eq!(line["target"], json!("some_dependency::module"));
    }

    #[test]
    fn omits_log_crate_metadata_fields() {
        let line = json_lines(|| {
            let record = log::Record::builder()
                .args(format_args!("from log"))
                .level(log::Level::Info)
                .target("some_dependency::module")
                .build();
            tracing_log::format_trace(&record).expect("bridged log record");
        })
        .remove(0);
        assert!(!line.keys().any(|key| key.starts_with("log.")));
    }

    #[test]
    fn omits_trace_and_span_ids_outside_span() {
        let line = json_lines(|| tracing::info!("no span")).remove(0);
        assert!(!line.contains_key("trace_id") && !line.contains_key("span_id"));
    }
}
