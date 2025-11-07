mod sampler;

use crate::Config;
use crate::filtering_exporter::FilteringExporter;
use crate::logging::Logging;
use crate::telemetry::sampler::SamplerOption;
use apollo_mcp_server::generated::telemetry::TelemetryAttribute;
use opentelemetry::{Key, KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::{Instrument, Stream, Temporality};
use opentelemetry_sdk::{
    Resource,
    metrics::{MeterProviderBuilder, PeriodicReader, SdkMeterProvider},
    propagation::TraceContextPropagator,
    trace::{RandomIdGenerator, SdkTracerProvider},
};
use opentelemetry_semantic_conventions::{
    SCHEMA_URL,
    attribute::{DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_VERSION},
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;
use tracing_opentelemetry::{MetricsLayer, OpenTelemetryLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Telemetry related options
#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct Telemetry {
    exporters: Option<Exporters>,
    service_name: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Exporters {
    metrics: Option<MetricsExporters>,
    tracing: Option<TracingExporters>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetricsExporters {
    otlp: Option<OTLPMetricExporter>,
    omitted_attributes: Option<HashSet<TelemetryAttribute>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub enum MetricTemporality {
    Cumulative,
    Delta,
}

impl OTLPMetricExporter {
    pub fn to_temporality(&self) -> Temporality {
        match self
            .temporality
            .as_ref()
            .unwrap_or(&MetricTemporality::Cumulative)
        {
            MetricTemporality::Cumulative => Temporality::Cumulative,
            MetricTemporality::Delta => Temporality::Delta,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OTLPMetricExporter {
    endpoint: String,
    protocol: String,
    temporality: Option<MetricTemporality>,
}

impl Default for OTLPMetricExporter {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4317".into(),
            protocol: "grpc".into(),
            temporality: Some(MetricTemporality::Cumulative),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TracingExporters {
    otlp: Option<OTLPTracingExporter>,
    sampler: Option<SamplerOption>,
    omitted_attributes: Option<HashSet<TelemetryAttribute>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OTLPTracingExporter {
    endpoint: String,
    protocol: String,
}

impl Default for OTLPTracingExporter {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4317".into(),
            protocol: "grpc".into(),
        }
    }
}

fn resource(telemetry: &Telemetry) -> Resource {
    let service_name = telemetry
        .service_name
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_NAME").to_string());

    let service_version = telemetry
        .version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let deployment_env = std::env::var("ENVIRONMENT").unwrap_or_else(|_| "development".to_string());

    Resource::builder()
        .with_service_name(service_name)
        .with_schema_url(
            [
                KeyValue::new(SERVICE_VERSION, service_version),
                KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, deployment_env),
            ],
            SCHEMA_URL,
        )
        .build()
}

fn init_meter_provider(telemetry: &Telemetry) -> Result<SdkMeterProvider, anyhow::Error> {
    let metrics_exporters = telemetry
        .exporters
        .as_ref()
        .and_then(|exporters| exporters.metrics.as_ref());

    let otlp = metrics_exporters
        .and_then(|metrics_exporters| metrics_exporters.otlp.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!("No metrics exporters configured, at least one is required")
        })?;

    let exporter = match otlp.protocol.as_str() {
        "grpc" => opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(otlp.endpoint.clone())
            .with_temporality(otlp.to_temporality())
            .build()?,
        "http/protobuf" => opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(otlp.endpoint.clone())
            .with_temporality(otlp.to_temporality())
            .build()?,
        other => {
            return Err(anyhow::anyhow!(
                "Unsupported OTLP protocol: {other}. Supported protocols are: grpc, http/protobuf"
            ));
        }
    };

    let omitted_attributes: HashSet<TelemetryAttribute> = metrics_exporters
        .and_then(|exporters| exporters.omitted_attributes.clone())
        .unwrap_or_default();
    let included_attributes: Vec<Key> = TelemetryAttribute::included_attributes(omitted_attributes)
        .iter()
        .map(|a| a.to_key())
        .collect();

    let reader = PeriodicReader::builder(exporter)
        .with_interval(std::time::Duration::from_secs(30))
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
        .with_resource(resource(telemetry))
        .with_reader(reader)
        .with_view(filtered_view)
        .build();

    Ok(meter_provider)
}

fn init_tracer_provider(telemetry: &Telemetry) -> Result<SdkTracerProvider, anyhow::Error> {
    let tracer_exporters = telemetry
        .exporters
        .as_ref()
        .and_then(|exporters| exporters.tracing.as_ref());

    let otlp = tracer_exporters
        .and_then(|tracing_exporters| tracing_exporters.otlp.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!("No tracing exporters configured, at least one is required")
        })?;

    let exporter = match otlp.protocol.as_str() {
        "grpc" => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(otlp.endpoint.clone())
            .build()?,
        "http/protobuf" => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(otlp.endpoint.clone())
            .build()?,
        other => {
            return Err(anyhow::anyhow!(
                "Unsupported OTLP protocol: {other}. Supported protocols are: grpc, http/protobuf"
            ));
        }
    };

    let sampler: opentelemetry_sdk::trace::Sampler = tracer_exporters
        .as_ref()
        .and_then(|e| e.sampler.clone())
        .unwrap_or_default()
        .into();

    let omitted_attributes: HashSet<Key> = tracer_exporters
        .and_then(|exporters| exporters.omitted_attributes.clone())
        .map(|set| set.iter().map(|a| a.to_key()).collect())
        .unwrap_or_default();

    let filtering_exporter = FilteringExporter::new(exporter, omitted_attributes);

    let tracer_provider = SdkTracerProvider::builder()
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource(telemetry))
        .with_batch_exporter(filtering_exporter)
        .with_sampler(sampler)
        .build();

    Ok(tracer_provider)
}

/// Initialize tracing-subscriber and return TelemetryGuard for logging and opentelemetry-related termination processing
pub fn init_tracing_subscriber(config: &Config) -> Result<TelemetryGuard, anyhow::Error> {
    let tracer_provider = if let Some(exporters) = &config.telemetry.exporters {
        if let Some(_tracing_exporters) = &exporters.tracing {
            init_tracer_provider(&config.telemetry)?
        } else {
            SdkTracerProvider::builder().build()
        }
    } else {
        SdkTracerProvider::builder().build()
    };
    let meter_provider = if let Some(exporters) = &config.telemetry.exporters {
        if let Some(_metrics_exporters) = &exporters.metrics {
            init_meter_provider(&config.telemetry)?
        } else {
            SdkMeterProvider::builder().build()
        }
    } else {
        SdkMeterProvider::builder().build()
    };
    let env_filter = Logging::env_filter(&config.logging)?;
    let (logging_layer, logging_guard) = Logging::logging_layer(&config.logging)?;

    let tracer = tracer_provider.tracer("apollo-mcp-trace");

    global::set_meter_provider(meter_provider.clone());
    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(tracer_provider.clone());

    tracing_subscriber::registry()
        .with(logging_layer)
        .with(env_filter)
        .with(MetricsLayer::new(meter_provider.clone()))
        .with(OpenTelemetryLayer::new(tracer))
        .try_init()?;

    Ok(TelemetryGuard {
        tracer_provider,
        meter_provider,
        logging_guard,
    })
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
                otlp: Some(OTLPMetricExporter::default()),
                omitted_attributes: None,
            }),
            Some(TracingExporters {
                otlp: Some(OTLPTracingExporter::default()),
                sampler: Default::default(),
                omitted_attributes: Some(ommitted),
            }),
        );
        // init_tracing_subscriber can only be called once in the test suite to avoid
        // panic when calling global::set_tracer_provider multiple times
        let guard = init_tracing_subscriber(&config);
        assert!(guard.is_ok());
    }

    #[tokio::test]
    async fn unknown_protocol_raises_meter_provider_error() {
        let config = test_config(
            None,
            None,
            Some(MetricsExporters {
                otlp: Some(OTLPMetricExporter {
                    protocol: "bogus".to_string(),
                    endpoint: "http://localhost:4317".to_string(),
                    temporality: None,
                }),
                omitted_attributes: None,
            }),
            None,
        );
        let result = init_meter_provider(&config.telemetry);
        assert!(
            result
                .err()
                .map(|e| e.to_string().contains("Unsupported OTLP protocol"))
                .unwrap_or(false)
        );
    }

    #[tokio::test]
    async fn http_protocol_returns_valid_meter_provider() {
        let config = test_config(
            None,
            None,
            Some(MetricsExporters {
                otlp: Some(OTLPMetricExporter {
                    protocol: "http/protobuf".to_string(),
                    endpoint: "http://localhost:4318/v1/metrics".to_string(),
                    temporality: Some(MetricTemporality::Delta),
                }),
                omitted_attributes: None,
            }),
            None,
        );
        let result = init_meter_provider(&config.telemetry);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn unknown_protocol_raises_tracer_provider_error() {
        let config = test_config(
            None,
            None,
            None,
            Some(TracingExporters {
                otlp: Some(OTLPTracingExporter {
                    protocol: "bogus".to_string(),
                    endpoint: "http://localhost:4317".to_string(),
                }),
                sampler: Default::default(),
                omitted_attributes: None,
            }),
        );
        let result = init_tracer_provider(&config.telemetry);
        assert!(
            result
                .err()
                .map(|e| e.to_string().contains("Unsupported OTLP protocol"))
                .unwrap_or(false)
        );
    }

    #[tokio::test]
    async fn http_protocol_returns_valid_tracer_provider() {
        let config = test_config(
            None,
            None,
            None,
            Some(TracingExporters {
                otlp: Some(OTLPTracingExporter {
                    protocol: "http/protobuf".to_string(),
                    endpoint: "http://localhost:4318/v1/traces".to_string(),
                }),
                sampler: Default::default(),
                omitted_attributes: None,
            }),
        );
        let result = init_tracer_provider(&config.telemetry);
        assert!(result.is_ok());
    }
}
