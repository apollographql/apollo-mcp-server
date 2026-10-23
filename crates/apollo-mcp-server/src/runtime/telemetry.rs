mod sampler;

use crate::runtime::Config;
use crate::runtime::filtering_exporter::FilteringExporter;
use crate::runtime::logging::{ConsoleStream, Logging};
use crate::runtime::telemetry::sampler::SamplerOption;
use apollo_mcp_server::generated::telemetry::TelemetryAttribute;
use apollo_mcp_server::server::w3c_text_map_propagator;
use opentelemetry::{Key, KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::tonic_types::metadata::MetadataMap;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::metrics::{Instrument, Stream, Temporality};
use opentelemetry_sdk::{
    Resource,
    metrics::{MeterProviderBuilder, PeriodicReader, SdkMeterProvider},
    resource::EnvResourceDetector,
    trace::{RandomIdGenerator, SdkTracerProvider},
};
use opentelemetry_semantic_conventions::{
    SCHEMA_URL,
    attribute::{DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_NAME, SERVICE_VERSION},
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing_opentelemetry::{MetricsLayer, OpenTelemetryLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Telemetry related options
#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Telemetry {
    exporters: Option<Exporters>,
    service_name: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Exporters {
    metrics: Option<MetricsExporters>,
    tracing: Option<TracingExporters>,
}

/// Metric telemetry exporter options
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "protocol", rename_all = "lowercase", deny_unknown_fields)]
pub enum MetricTelemetryExporter {
    /// GRPC Exporter
    Grpc {
        endpoint: String,
        #[serde(default)]
        temporality: MetricTemporality,
        #[serde(default, deserialize_with = "parsers::metadata_map_from_str")]
        #[schemars(schema_with = "super::schemas::header_map")]
        metadata: MetadataMap,
    },

    /// Http/protobuf exporter
    #[serde(rename = "http/protobuf")]
    HttpProtobuf {
        endpoint: String,
        #[serde(default)]
        temporality: MetricTemporality,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

/// Trace telemetry exporter options
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "protocol", rename_all = "lowercase", deny_unknown_fields)]
pub enum TraceTelemetryExporter {
    /// GRPC Exporter
    Grpc {
        endpoint: String,
        #[serde(default, deserialize_with = "parsers::metadata_map_from_str")]
        #[schemars(schema_with = "super::schemas::header_map")]
        metadata: MetadataMap,
    },

    /// Http/protobuf exporter
    #[serde(rename = "http/protobuf")]
    HttpProtobuf {
        endpoint: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricsExporters {
    otlp: Option<MetricTelemetryExporter>,
    /// Apollo metric attributes excluded before export.
    omitted_attributes: Option<HashSet<TelemetryAttribute>>,
    /// Interval at which metrics are exported.
    ///
    /// Accepts human-readable durations (e.g., "30s", "1m", "5m").
    /// Defaults to 30 seconds when not specified.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>")]
    export_interval: Option<Duration>,
}

#[derive(Debug, Default, JsonSchema, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MetricTemporality {
    #[default]
    Cumulative,
    Delta,
    LowMemory,
}

impl<'de> Deserialize<'de> for MetricTemporality {
    /// Case insensitive deserializer for str to MetricTemporality
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "cumulative" => Ok(Self::Cumulative),
            "delta" => Ok(Self::Delta),
            "lowmemory" | "low_memory" => Ok(Self::LowMemory),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["cumulative", "delta", "lowmemory"],
            )),
        }
    }
}

impl From<&MetricTemporality> for Temporality {
    fn from(value: &MetricTemporality) -> Self {
        match value {
            MetricTemporality::Cumulative => Temporality::Cumulative,
            MetricTemporality::Delta => Temporality::Delta,
            MetricTemporality::LowMemory => Temporality::LowMemory,
        }
    }
}

impl Default for MetricTelemetryExporter {
    fn default() -> Self {
        Self::Grpc {
            endpoint: "http://localhost:4317".into(),
            temporality: MetricTemporality::default(),
            metadata: MetadataMap::default(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TracingExporters {
    otlp: Option<TraceTelemetryExporter>,
    sampler: Option<SamplerOption>,
    /// Apollo span attributes excluded before export.
    omitted_attributes: Option<HashSet<TelemetryAttribute>>,
    /// Export the OpenTelemetry GenAI (`gen_ai.*`) span attributes, including
    /// tool call arguments and results, which may contain sensitive data.
    /// Defaults to off.
    gen_ai: Option<bool>,
}

impl Telemetry {
    /// Whether the tracing exporter exports the OpenTelemetry GenAI attributes.
    pub fn gen_ai(&self) -> bool {
        self.exporters
            .as_ref()
            .and_then(|exporters| exporters.tracing.as_ref())
            .is_some_and(TracingExporters::gen_ai)
    }
}

impl TracingExporters {
    fn gen_ai(&self) -> bool {
        self.gen_ai.unwrap_or_default()
    }

    /// Span attribute keys the exporter drops: every key of an omitted
    /// attribute, plus the GenAI attributes unless they're enabled.
    fn omitted_span_keys(&self) -> HashSet<Key> {
        let mut keys: HashSet<Key> = self
            .omitted_attributes
            .iter()
            .flatten()
            .flat_map(|a| a.span_keys())
            .collect();
        if !self.gen_ai() {
            keys.extend(TelemetryAttribute::GEN_AI_KEYS);
        }
        keys
    }
}

impl Default for TraceTelemetryExporter {
    fn default() -> Self {
        Self::Grpc {
            endpoint: "http://localhost:4317".into(),
            metadata: MetadataMap::default(),
        }
    }
}

mod parsers {
    use opentelemetry_otlp::tonic_types::metadata::MetadataMap;
    use serde::Deserializer;
    use std::str::FromStr;
    use tonic::metadata::{MetadataKey, MetadataValue};

    pub(super) fn metadata_map_from_str<'de, D>(deserializer: D) -> Result<MetadataMap, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MapFromStrVisitor;
        impl<'de> serde::de::Visitor<'de> for MapFromStrVisitor {
            type Value = MetadataMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a map of header string keys and values")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut parsed = MetadataMap::with_capacity(map.size_hint().unwrap_or(0));

                // While there are entries remaining in the input, add them
                // into our map.
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    let key = MetadataKey::from_str(&key)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))?;
                    let value = MetadataValue::from_str(&value)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))?;

                    parsed.insert(key, value);
                }

                Ok(parsed)
            }
        }

        deserializer.deserialize_map(MapFromStrVisitor)
    }
}

/// A resource attribute set both by our config and by the OpenTelemetry environment variables
/// (`OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`) to different values.
#[derive(Debug, PartialEq)]
struct ResourceConflict {
    attribute: &'static str,
    source: &'static str,
    configured: String,
    detected: String,
}

/// Builds the resource shared by traces and metrics.
///
/// Each attribute we set resolves in the same order: our config, then the OpenTelemetry
/// environment variables, then our default. `deployment.environment.name` has no default.
/// Keys we don't set (e.g. `host.name`) pass through from `OTEL_RESOURCE_ATTRIBUTES`.
fn resource(telemetry: &Telemetry) -> (Resource, Vec<ResourceConflict>) {
    // Per the OpenTelemetry spec, `OTEL_SERVICE_NAME` beats `service.name` in
    // `OTEL_RESOURCE_ATTRIBUTES`. `Resource::builder()` gets this backwards, so every value we
    // resolve below is set explicitly rather than left to its detectors.
    let mut detected =
        Resource::builder_empty().with_detector(Box::new(EnvResourceDetector::new()));
    if let Some(service_name) = non_empty_env_var("OTEL_SERVICE_NAME") {
        detected = detected.with_service_name(service_name);
    }
    let detected = detected.build();

    let candidates = [
        (
            SERVICE_NAME,
            "telemetry.service_name",
            telemetry.service_name.clone(),
            Some(env!("CARGO_PKG_NAME")),
        ),
        (
            SERVICE_VERSION,
            "telemetry.version",
            telemetry.version.clone(),
            Some(env!("CARGO_PKG_VERSION")),
        ),
        (
            DEPLOYMENT_ENVIRONMENT_NAME,
            "ENVIRONMENT",
            non_empty_env_var("ENVIRONMENT"),
            None,
        ),
    ];

    let mut attributes = Vec::new();
    let mut conflicts = Vec::new();
    for (attribute, source, configured, default) in candidates {
        let detected = detected
            .get(&Key::from_static_str(attribute))
            .map(|value| value.as_str().into_owned());
        let value = match (configured, detected) {
            (Some(configured), Some(detected)) => {
                if configured != detected {
                    conflicts.push(ResourceConflict {
                        attribute,
                        source,
                        configured: configured.clone(),
                        detected,
                    });
                }
                Some(configured)
            }
            (configured, detected) => configured.or(detected).or(default.map(String::from)),
        };
        if let Some(value) = value {
            attributes.push(KeyValue::new(attribute, value));
        }
    }

    // `with_attributes` lets our values override the detected ones, unlike `with_schema_url`,
    // which merges the detected resource on top, so pass it no attributes.
    let resource = Resource::builder()
        .with_attributes(attributes)
        .with_schema_url([], SCHEMA_URL)
        .build();

    (resource, conflicts)
}

fn non_empty_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn warn_resource_conflicts(conflicts: &[ResourceConflict]) {
    for conflict in conflicts {
        tracing::warn!(
            attribute = conflict.attribute,
            configured = %conflict.configured,
            detected = %conflict.detected,
            "{} overrides the value from OpenTelemetry environment variables",
            conflict.source,
        );
    }
}

fn init_meter_provider(
    telemetry: &Telemetry,
    resource: Resource,
) -> Result<SdkMeterProvider, anyhow::Error> {
    let metrics_exporters = telemetry
        .exporters
        .as_ref()
        .and_then(|exporters| exporters.metrics.as_ref());

    let otlp = metrics_exporters
        .and_then(|metrics_exporters| metrics_exporters.otlp.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!("No metrics exporters configured, at least one is required")
        })?;

    let exporter = match otlp {
        MetricTelemetryExporter::Grpc {
            endpoint,
            temporality,
            metadata,
        } => opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .with_temporality(temporality.into())
            .with_metadata(metadata.clone())
            .build()?,
        MetricTelemetryExporter::HttpProtobuf {
            endpoint,
            temporality,
            headers,
        } => opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .with_temporality(temporality.into())
            .with_headers(headers.clone())
            .build()?,
    };

    let omitted_attributes: HashSet<TelemetryAttribute> = metrics_exporters
        .as_ref()
        .and_then(|exporters| exporters.omitted_attributes.clone())
        .unwrap_or_default();
    let included_attributes: Vec<Key> = TelemetryAttribute::included_attributes(omitted_attributes)
        .iter()
        .map(|a| a.to_key())
        .collect();

    let export_interval = metrics_exporters
        .as_ref()
        .and_then(|exporters| exporters.export_interval)
        .unwrap_or(Duration::from_secs(30));

    let reader = PeriodicReader::builder(exporter)
        .with_interval(export_interval)
        .build();

    let filtered_view = move |i: &Instrument| {
        if i.name().starts_with("apollo.") {
            Stream::builder()
                .with_allowed_attribute_keys(included_attributes.clone()) // if available in your version
                .build()
                .ok()
        } else {
            None
        }
    };

    let meter_provider = MeterProviderBuilder::default()
        .with_resource(resource)
        .with_reader(reader)
        .with_view(filtered_view)
        .build();

    Ok(meter_provider)
}

fn init_tracer_provider(
    telemetry: &Telemetry,
    resource: Resource,
) -> Result<SdkTracerProvider, anyhow::Error> {
    let tracer_exporters = telemetry
        .exporters
        .as_ref()
        .and_then(|exporters| exporters.tracing.as_ref());

    let otlp = tracer_exporters
        .and_then(|tracing_exporters| tracing_exporters.otlp.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!("No tracing exporters configured, at least one is required")
        })?;

    let exporter = match otlp {
        TraceTelemetryExporter::Grpc {
            endpoint, metadata, ..
        } => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .with_metadata(metadata.clone())
            .build()?,
        TraceTelemetryExporter::HttpProtobuf {
            endpoint, headers, ..
        } => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .with_headers(headers.clone())
            .build()?,
    };

    let sampler: opentelemetry_sdk::trace::Sampler = tracer_exporters
        .as_ref()
        .and_then(|e| e.sampler.clone())
        .unwrap_or_default()
        .into();

    let omitted_attributes = tracer_exporters
        .map(TracingExporters::omitted_span_keys)
        .unwrap_or_default();

    let filtering_exporter = FilteringExporter::new(exporter, omitted_attributes);

    let tracer_provider = SdkTracerProvider::builder()
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .with_batch_exporter(filtering_exporter)
        .with_sampler(sampler)
        .build();

    Ok(tracer_provider)
}

/// Initialize tracing-subscriber and return TelemetryGuard for logging and opentelemetry-related termination processing
pub fn init_tracing_subscriber(config: &Config) -> Result<TelemetryGuard, anyhow::Error> {
    let exporters = config.telemetry.exporters.as_ref();
    let tracing_enabled = exporters.is_some_and(|exporters| exporters.tracing.is_some());
    let metrics_enabled = exporters.is_some_and(|exporters| exporters.metrics.is_some());

    let (resource, resource_conflicts) = resource(&config.telemetry);
    let tracer_provider = if tracing_enabled {
        init_tracer_provider(&config.telemetry, resource.clone())?
    } else {
        SdkTracerProvider::builder().build()
    };
    let meter_provider = if metrics_enabled {
        init_meter_provider(&config.telemetry, resource)?
    } else {
        SdkMeterProvider::builder().build()
    };
    let env_filter = Logging::env_filter(&config.logging)?;
    let (logging_layer, logging_guard) = Logging::logging_layer(
        &config.logging,
        ConsoleStream::for_transport(&config.transport),
    )?;

    let tracer = tracer_provider.tracer("apollo-mcp-trace");

    global::set_meter_provider(meter_provider.clone());
    global::set_text_map_propagator(w3c_text_map_propagator());
    global::set_tracer_provider(tracer_provider.clone());

    tracing_subscriber::registry()
        .with(logging_layer)
        .with(tracing_subscriber::filter::filter_fn(|metadata| {
            rmcp_event_allowed(
                SUPPRESS_RMCP_DEBUG.load(Ordering::Relaxed),
                metadata.target(),
                metadata.level(),
            )
        }))
        .with(env_filter)
        .with(MetricsLayer::new(meter_provider.clone()))
        .with(OpenTelemetryLayer::new(tracer))
        .try_init()?;

    if tracing_enabled || metrics_enabled {
        warn_resource_conflicts(&resource_conflicts);
    }

    Ok(TelemetryGuard {
        tracer_provider,
        meter_provider,
        logging_guard,
    })
}

// rmcp debug/trace events include complete request and response payloads,
// including bearer task IDs. This is a hard filter rather than an overridable
// RUST_LOG directive. It is set by any server build with Tasks enabled and never
// cleared, so a config reload that enables Tasks reaches the startup subscriber.
static SUPPRESS_RMCP_DEBUG: AtomicBool = AtomicBool::new(false);

/// Suppress rmcp debug and trace events for the rest of the process.
pub fn suppress_rmcp_debug() {
    if !SUPPRESS_RMCP_DEBUG.swap(true, Ordering::Relaxed) {
        // `FilterFn` caches each callsite's interest on first registration.
        tracing::callsite::rebuild_interest_cache();
    }
}

fn rmcp_event_allowed(suppress: bool, target: &str, level: &tracing::Level) -> bool {
    let rmcp = target == "rmcp" || target.starts_with("rmcp::");
    !suppress || !rmcp || *level <= tracing::Level::INFO
}

pub struct TelemetryGuard {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logging_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Err(err) = self.tracer_provider.shutdown() {
            tracing::error!("{err:?}");
        }
        if let Err(err) = self.meter_provider.shutdown() {
            tracing::error!("{err:?}");
        }
        drop(self.logging_guard.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, HeaderValue};
    use rstest::rstest;
    use serde::de::value::{Error, MapDeserializer, StrDeserializer};
    use tonic::metadata::MetadataValue;

    #[rstest]
    #[case::rmcp_debug_suppressed(true, "rmcp::service", tracing::Level::DEBUG, false)]
    #[case::rmcp_trace_suppressed(true, "rmcp", tracing::Level::TRACE, false)]
    #[case::rmcp_info_kept(true, "rmcp::service", tracing::Level::INFO, true)]
    #[case::rmcp_warn_kept(true, "rmcp::service", tracing::Level::WARN, true)]
    #[case::other_debug_kept(true, "apollo_mcp_server", tracing::Level::DEBUG, true)]
    #[case::rmcp_prefixed_crate_kept(true, "rmcp_macros", tracing::Level::DEBUG, true)]
    #[case::unsuppressed_rmcp_debug_kept(false, "rmcp::service", tracing::Level::DEBUG, true)]
    fn rmcp_event_filter(
        #[case] suppress: bool,
        #[case] target: &str,
        #[case] level: tracing::Level,
        #[case] allowed: bool,
    ) {
        assert_eq!(rmcp_event_allowed(suppress, target, &level), allowed);
    }

    #[test]
    fn rmcp_debug_suppression_is_sticky() {
        suppress_rmcp_debug();
        suppress_rmcp_debug();
        assert!(SUPPRESS_RMCP_DEBUG.load(Ordering::Relaxed));
    }

    fn tracing_exporters(omitted: &[TelemetryAttribute], gen_ai: Option<bool>) -> TracingExporters {
        TracingExporters {
            otlp: None,
            sampler: None,
            omitted_attributes: Some(omitted.iter().copied().collect()),
            gen_ai,
        }
    }

    const ALL_GEN_AI: &[&str] = &[
        "gen_ai.operation.name",
        "gen_ai.tool.name",
        "gen_ai.tool.call.arguments",
        "gen_ai.tool.call.result",
    ];

    #[rstest]
    #[case::unset_drops_gen_ai(&[], None, ALL_GEN_AI)]
    #[case::disabled_drops_gen_ai(&[], Some(false), ALL_GEN_AI)]
    #[case::enabled_keeps_gen_ai(&[], Some(true), &[])]
    #[case::omitted_attribute_overrides_gen_ai(&[TelemetryAttribute::ToolArguments], Some(true), &["apollo.mcp.tool_arguments", "gen_ai.tool.call.arguments"])]
    fn omitted_span_keys_respect_gen_ai(
        #[case] omitted: &[TelemetryAttribute],
        #[case] gen_ai: Option<bool>,
        #[case] expected: &[&'static str],
    ) {
        let keys = tracing_exporters(omitted, gen_ai).omitted_span_keys();
        let expected: HashSet<Key> = expected.iter().map(|k| Key::from_static_str(k)).collect();
        assert_eq!(keys, expected);
    }

    fn test_config(
        service_name: Option<&str>,
        version: Option<&str>,
        metrics: Option<MetricsExporters>,
        tracing: Option<TracingExporters>,
    ) -> Config {
        Config {
            telemetry: Telemetry {
                exporters: Some(Exporters { metrics, tracing }),
                service_name: service_name.map(|s| s.to_string()),
                version: version.map(|v| v.to_string()),
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn guard_is_provided_when_tracing_configured() {
        let mut ommitted = HashSet::new();
        ommitted.insert(TelemetryAttribute::RequestId);

        let config = test_config(
            Some("test-config"),
            Some("1.0.0"),
            Some(MetricsExporters {
                otlp: Some(MetricTelemetryExporter::default()),
                omitted_attributes: None,
                export_interval: None,
            }),
            Some(TracingExporters {
                otlp: Some(TraceTelemetryExporter::default()),
                sampler: Some(SamplerOption::default()),
                omitted_attributes: Some(ommitted),
                gen_ai: None,
            }),
        );
        // init_tracing_subscriber can only be called once in the test suite to avoid
        // panic when calling global::set_tracer_provider multiple times
        let guard = init_tracing_subscriber(&config);
        assert!(guard.is_ok());
    }

    #[test]
    fn http_protocol_returns_valid_meter_provider() {
        let config = test_config(
            None,
            None,
            Some(MetricsExporters {
                otlp: Some(MetricTelemetryExporter::HttpProtobuf {
                    endpoint: "http://localhost:4318/v1/metrics".to_string(),
                    temporality: MetricTemporality::Delta,
                    headers: HashMap::new(),
                }),
                omitted_attributes: None,
                export_interval: None,
            }),
            None,
        );
        let result = init_meter_provider(&config.telemetry, Resource::builder_empty().build());
        assert!(result.is_ok());
    }

    #[test]
    fn http_protocol_returns_valid_tracer_provider() {
        let config = test_config(
            None,
            None,
            None,
            Some(TracingExporters {
                otlp: Some(TraceTelemetryExporter::HttpProtobuf {
                    endpoint: "http://localhost:4318/v1/traces".to_string(),
                    headers: HashMap::new(),
                }),
                sampler: Some(SamplerOption::default()),
                omitted_attributes: None,
                gen_ai: None,
            }),
        );
        let result = init_tracer_provider(&config.telemetry, Resource::builder_empty().build());
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn grpc_trace_exporter_with_metadata_returns_valid_tracer_provider() {
        let mut header_map = HeaderMap::new();
        header_map.insert("key", HeaderValue::from_static("value"));

        let config = test_config(
            None,
            None,
            None,
            Some(TracingExporters {
                otlp: Some(TraceTelemetryExporter::Grpc {
                    endpoint: "http://localhost:4318/v1/traces".to_string(),
                    metadata: MetadataMap::from_headers(header_map),
                }),
                sampler: Some(SamplerOption::default()),
                omitted_attributes: None,
                gen_ai: None,
            }),
        );
        let result = init_tracer_provider(&config.telemetry, Resource::builder_empty().build());
        assert!(result.is_ok());
    }

    #[test]
    fn http_protobuf_metric_exporter_with_headers_returns_valid_tracer_provider() {
        let mut header_map = HashMap::new();
        header_map.insert("key".to_string(), "value".to_string());

        let config = test_config(
            None,
            None,
            None,
            Some(TracingExporters {
                otlp: Some(TraceTelemetryExporter::HttpProtobuf {
                    endpoint: "http://localhost:4318/v1/traces".to_string(),
                    headers: header_map,
                }),
                sampler: Some(SamplerOption::default()),
                omitted_attributes: None,
                gen_ai: None,
            }),
        );
        let result = init_tracer_provider(&config.telemetry, Resource::builder_empty().build());
        assert!(result.is_ok());
    }

    #[rstest]
    #[case::lower_cumulative("cumulative", MetricTemporality::Cumulative)]
    #[case::title_cumulative("Cumulative", MetricTemporality::Cumulative)]
    #[case::upper_cumulative("CUMULATIVE", MetricTemporality::Cumulative)]
    #[case::lower_delta("delta", MetricTemporality::Delta)]
    #[case::title_delta("Delta", MetricTemporality::Delta)]
    #[case::upper_delta("DELTA", MetricTemporality::Delta)]
    #[case::lower_lowmemory("lowmemory", MetricTemporality::LowMemory)]
    #[case::title_lowmemory("LowMemory", MetricTemporality::LowMemory)]
    #[case::upper_lowmemory("LOWMEMORY", MetricTemporality::LowMemory)]
    fn direct_deserialization_deserializes_into_the_correct_temporality(
        #[case] value: &str,
        #[case] expected: MetricTemporality,
    ) {
        let de = StrDeserializer::<Error>::new(value);
        let actual: MetricTemporality = MetricTemporality::deserialize(de).unwrap();
        assert_eq!(actual, expected);
    }

    #[rstest]
    #[case::lower_cumulative("cumulative", MetricTemporality::Cumulative)]
    #[case::title_cumulative("Cumulative", MetricTemporality::Cumulative)]
    #[case::upper_cumulative("CUMULATIVE", MetricTemporality::Cumulative)]
    #[case::lower_delta("delta", MetricTemporality::Delta)]
    #[case::title_delta("Delta", MetricTemporality::Delta)]
    #[case::upper_delta("DELTA", MetricTemporality::Delta)]
    #[case::lower_lowmemory("lowmemory", MetricTemporality::LowMemory)]
    #[case::title_lowmemory("LowMemory", MetricTemporality::LowMemory)]
    #[case::upper_lowmemory("LOWMEMORY", MetricTemporality::LowMemory)]
    fn yaml_deserialization_deserializes_into_the_correct_temporality(
        #[case] yaml_value: &str,
        #[case] expected: MetricTemporality,
    ) {
        let actual: MetricTemporality = serde_yaml::from_str(yaml_value).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn yaml_deserialization_of_invalid_value_results_in_an_unknown_variant_error() {
        let result: Result<MetricTemporality, _> = serde_yaml::from_str("invalid");
        assert!(result.is_err());
        assert!(
            result
                .expect_err("expected an error for invalid temporality")
                .to_string()
                .contains("unknown variant")
        );
    }

    #[test]
    fn direct_deserialization_of_invalid_value_results_in_an_unknown_variant_error() {
        let de = StrDeserializer::<Error>::new("invalid");
        let err = MetricTemporality::deserialize(de).unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn deserializes_into_a_metadata_map() {
        let de = MapDeserializer::<_, Error>::new(
            vec![
                ("some-key".to_string(), "some-value".to_string()),
                ("another-key".to_string(), "another-value".to_string()),
            ]
            .into_iter(),
        );

        let map: MetadataMap = parsers::metadata_map_from_str(de).unwrap();

        assert_eq!(
            map.get("some-key"),
            Some(&MetadataValue::try_from("some-value").unwrap())
        );
        assert_eq!(
            map.get("another-key"),
            Some(&MetadataValue::try_from("another-value").unwrap())
        );
    }

    #[test]
    fn yaml_key_value_pairs_deserialize_into_a_metadata_map() {
        let y = r#"
          exporters:
            metrics:
              otlp:
                protocol: grpc
                endpoint: "http://127.0.0.1:4317"
                metadata:
                  some-key: some-value
                  another-key: another-value
        "#;

        let telemetry_config: Telemetry = serde_yaml::from_str(y).unwrap();
        let metric_exporter = telemetry_config
            .exporters
            .unwrap()
            .metrics
            .unwrap()
            .otlp
            .unwrap();

        match metric_exporter {
            MetricTelemetryExporter::Grpc { metadata, .. } => {
                assert_eq!(
                    metadata.get("some-key"),
                    Some(&MetadataValue::try_from("some-value").unwrap())
                );

                assert_eq!(
                    metadata.get("another-key"),
                    Some(&MetadataValue::try_from("another-value").unwrap())
                );
            }
            MetricTelemetryExporter::HttpProtobuf { .. } => {
                panic!("unexpected protocol")
            }
        }
    }

    #[test]
    fn yaml_deserialization_with_export_interval() {
        let y = r#"
          exporters:
            metrics:
              otlp:
                protocol: grpc
                endpoint: "http://127.0.0.1:4317"
              export_interval: 1m
        "#;

        let telemetry_config: Telemetry = serde_yaml::from_str(y).unwrap();
        let metrics_exporters = telemetry_config.exporters.unwrap().metrics.unwrap();

        assert_eq!(
            metrics_exporters.export_interval,
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn yaml_deserialization_without_export_interval_defaults_to_none() {
        let y = r#"
          exporters:
            metrics:
              otlp:
                protocol: grpc
                endpoint: "http://127.0.0.1:4317"
        "#;

        let telemetry_config: Telemetry = serde_yaml::from_str(y).unwrap();
        let metrics_exporters = telemetry_config.exporters.unwrap().metrics.unwrap();

        assert_eq!(metrics_exporters.export_interval, None);
    }

    #[test]
    fn meter_provider_with_custom_export_interval() {
        let config = test_config(
            None,
            None,
            Some(MetricsExporters {
                otlp: Some(MetricTelemetryExporter::HttpProtobuf {
                    endpoint: "http://localhost:4318/v1/metrics".to_string(),
                    temporality: MetricTemporality::Delta,
                    headers: HashMap::new(),
                }),
                omitted_attributes: None,
                export_interval: Some(Duration::from_secs(60)),
            }),
            None,
        );
        let result = init_meter_provider(&config.telemetry, Resource::builder_empty().build());
        assert!(result.is_ok());
    }

    #[test]
    fn w3c_propagator_extracts_and_injects_baggage_and_trace_context() {
        use opentelemetry::baggage::BaggageExt;
        use opentelemetry::propagation::TextMapPropagator;
        use opentelemetry::trace::TraceContextExt;
        use opentelemetry::{Context, KeyValue};
        use std::collections::HashMap;

        let propagator = w3c_text_map_propagator();
        let mut incoming = HashMap::new();
        incoming.insert(
            "traceparent".to_string(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
        );
        incoming.insert(
            "baggage".to_string(),
            "userId=alice,serverNode=DF28".to_string(),
        );

        let extracted = propagator.extract(&incoming);
        assert_eq!(
            extracted.baggage().get("userId").map(|v| v.as_str()),
            Some("alice")
        );
        assert_eq!(
            extracted.baggage().get("serverNode").map(|v| v.as_str()),
            Some("DF28")
        );
        assert_eq!(
            format!("{:032x}", extracted.span().span_context().trace_id()),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );

        let cx = Context::current().with_baggage(vec![KeyValue::new("userId", "alice")]);
        let mut outgoing = HashMap::new();
        propagator.inject_context(&cx, &mut outgoing);
        let baggage_header = outgoing
            .get("baggage")
            .expect("composite propagator should inject a baggage header");
        assert!(baggage_header.contains("userId=alice"));
    }

    fn telemetry_with(service_name: Option<&str>, version: Option<&str>) -> Telemetry {
        Telemetry {
            service_name: service_name.map(String::from),
            version: version.map(String::from),
            ..Default::default()
        }
    }

    /// `Jail` only restores the variables it sets, so blank the ones `resource` reads to keep the
    /// developer's or CI runner's environment out of the tests. Empty values count as unset.
    fn clear_resource_env(jail: &mut figment::Jail) {
        for name in [
            "ENVIRONMENT",
            "OTEL_SERVICE_NAME",
            "OTEL_RESOURCE_ATTRIBUTES",
        ] {
            jail.set_env(name, "");
        }
    }

    fn attribute_value(resource: &Resource, attribute: &'static str) -> Option<String> {
        resource
            .get(&Key::from_static_str(attribute))
            .map(|value| value.as_str().into_owned())
    }

    #[rstest]
    #[case::service_name_from_config(Some("ours"), None, &[], SERVICE_NAME, Some("ours"))]
    #[case::service_name_from_otel_service_name(None, None, &[("OTEL_SERVICE_NAME", "env")], SERVICE_NAME, Some("env"))]
    #[case::service_name_from_otel_resource_attributes(None, None, &[("OTEL_RESOURCE_ATTRIBUTES", "service.name=env")], SERVICE_NAME, Some("env"))]
    #[case::otel_service_name_beats_otel_resource_attributes(None, None, &[("OTEL_SERVICE_NAME", "a"), ("OTEL_RESOURCE_ATTRIBUTES", "service.name=b")], SERVICE_NAME, Some("a"))]
    #[case::service_name_config_beats_env(Some("ours"), None, &[("OTEL_SERVICE_NAME", "env")], SERVICE_NAME, Some("ours"))]
    #[case::service_name_defaults_to_package_name(None, None, &[], SERVICE_NAME, Some(env!("CARGO_PKG_NAME")))]
    #[case::service_version_from_config(None, Some("ours"), &[], SERVICE_VERSION, Some("ours"))]
    #[case::service_version_from_env(None, None, &[("OTEL_RESOURCE_ATTRIBUTES", "service.version=env")], SERVICE_VERSION, Some("env"))]
    #[case::service_version_config_beats_env(None, Some("ours"), &[("OTEL_RESOURCE_ATTRIBUTES", "service.version=env")], SERVICE_VERSION, Some("ours"))]
    #[case::service_version_defaults_to_package_version(None, None, &[], SERVICE_VERSION, Some(env!("CARGO_PKG_VERSION")))]
    #[case::environment_from_environment_var(None, None, &[("ENVIRONMENT", "ours")], DEPLOYMENT_ENVIRONMENT_NAME, Some("ours"))]
    #[case::environment_from_otel_resource_attributes(None, None, &[("OTEL_RESOURCE_ATTRIBUTES", "deployment.environment.name=env")], DEPLOYMENT_ENVIRONMENT_NAME, Some("env"))]
    #[case::environment_var_beats_otel_resource_attributes(None, None, &[("ENVIRONMENT", "ours"), ("OTEL_RESOURCE_ATTRIBUTES", "deployment.environment.name=env")], DEPLOYMENT_ENVIRONMENT_NAME, Some("ours"))]
    #[case::environment_omitted_when_unset(None, None, &[], DEPLOYMENT_ENVIRONMENT_NAME, None)]
    fn resource_resolves_attribute(
        #[case] service_name: Option<&str>,
        #[case] version: Option<&str>,
        #[case] env: &[(&str, &str)],
        #[case] attribute: &'static str,
        #[case] expected: Option<&str>,
    ) {
        figment::Jail::expect_with(|jail| {
            clear_resource_env(jail);
            for (name, value) in env {
                jail.set_env(name, value);
            }

            let (resource, _) = resource(&telemetry_with(service_name, version));

            assert_eq!(attribute_value(&resource, attribute).as_deref(), expected);
            Ok(())
        });
    }

    #[rstest]
    #[case::service_name(Some("ours"), None, &[("OTEL_SERVICE_NAME", "env")], SERVICE_NAME, "telemetry.service_name")]
    #[case::service_version(None, Some("ours"), &[("OTEL_RESOURCE_ATTRIBUTES", "service.version=env")], SERVICE_VERSION, "telemetry.version")]
    #[case::environment(None, None, &[("ENVIRONMENT", "ours"), ("OTEL_RESOURCE_ATTRIBUTES", "deployment.environment.name=env")], DEPLOYMENT_ENVIRONMENT_NAME, "ENVIRONMENT")]
    fn resource_reports_conflict_when_config_and_env_differ(
        #[case] service_name: Option<&str>,
        #[case] version: Option<&str>,
        #[case] env: &[(&str, &str)],
        #[case] attribute: &'static str,
        #[case] source: &'static str,
    ) {
        figment::Jail::expect_with(|jail| {
            clear_resource_env(jail);
            for (name, value) in env {
                jail.set_env(name, value);
            }

            let (_, conflicts) = resource(&telemetry_with(service_name, version));

            assert_eq!(
                conflicts,
                vec![ResourceConflict {
                    attribute,
                    source,
                    configured: "ours".to_string(),
                    detected: "env".to_string(),
                }]
            );
            Ok(())
        });
    }

    #[test]
    fn resource_reports_no_conflict_when_config_and_env_agree() {
        figment::Jail::expect_with(|jail| {
            clear_resource_env(jail);
            jail.set_env("OTEL_SERVICE_NAME", "same");
            jail.set_env("ENVIRONMENT", "same");
            jail.set_env(
                "OTEL_RESOURCE_ATTRIBUTES",
                "service.version=same,deployment.environment.name=same",
            );

            let (_, conflicts) = resource(&telemetry_with(Some("same"), Some("same")));

            assert!(conflicts.is_empty());
            Ok(())
        });
    }

    #[test]
    fn resource_passes_through_otel_resource_attributes_we_do_not_set() {
        figment::Jail::expect_with(|jail| {
            clear_resource_env(jail);
            jail.set_env("OTEL_RESOURCE_ATTRIBUTES", "team=x,host.name=y");

            let (resource, _) = resource(&telemetry_with(None, None));

            assert_eq!(
                (
                    attribute_value(&resource, "team").as_deref(),
                    attribute_value(&resource, "host.name").as_deref(),
                ),
                (Some("x"), Some("y"))
            );
            Ok(())
        });
    }

    #[test]
    fn resource_sets_schema_url() {
        let (resource, _) = resource(&telemetry_with(None, None));

        assert_eq!(resource.schema_url(), Some(SCHEMA_URL));
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // A scoped subscriber, because `guard_is_provided_when_tracing_configured` installs the
    // global one.
    #[test]
    fn warn_resource_conflicts_logs_both_values() {
        let logs = CapturedLogs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            warn_resource_conflicts(&[ResourceConflict {
                attribute: SERVICE_NAME,
                source: "telemetry.service_name",
                configured: "ours".to_string(),
                detected: "env".to_string(),
            }]);
        });

        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains(
            "telemetry.service_name overrides the value from OpenTelemetry environment variables attribute=\"service.name\" configured=ours detected=env"
        ));
    }
}
