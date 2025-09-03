use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
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
use tracing_opentelemetry::{MetricsLayer, OpenTelemetryLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::runtime::Config;
use crate::runtime::logging::Logging;

/// Telemetry related options
#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct Telemetry {
    exporters: Option<Exporters>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Exporters {
    metrics: Option<MetricsExporters>,
    tracing: Option<TracingExporters>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetricsExporters {
    otlp: Option<OTLPMetricExporter>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OTLPMetricExporter {
    endpoint: String,
    protocol: String,
}

impl Default for OTLPMetricExporter {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4317".into(),
            protocol: "grpc".into(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TracingExporters {
    otlp: Option<OTLPTracingExporter>,
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

fn resource() -> Resource {
    Resource::builder()
        .with_service_name(env!("CARGO_PKG_NAME"))
        .with_schema_url(
            [
                KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                KeyValue::new(
                    DEPLOYMENT_ENVIRONMENT_NAME,
                    std::env::var("ENVIRONMENT").unwrap_or_else(|_| "development".to_string()),
                ),
            ],
            SCHEMA_URL,
        )
        .build()
}

fn init_meter_provider(
    metric_exporters: &MetricsExporters,
) -> Result<SdkMeterProvider, anyhow::Error> {
    let otlp = metric_exporters.otlp.as_ref().ok_or_else(|| {
        anyhow::anyhow!("No metrics exporters configured, at least one is required")
    })?;
    let exporter = match otlp.protocol.as_str() {
        "grpc" => opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(otlp.endpoint.clone())
            .build()?,
        "http/protobuf" => opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(otlp.endpoint.clone())
            .build()?,
        other => {
            return Err(anyhow::anyhow!(
                "Unsupported OTLP protocol: {other}. Supported protocols are: grpc, http/protobuf"
            ));
        }
    };

    let reader = PeriodicReader::builder(exporter)
        .with_interval(std::time::Duration::from_secs(30))
        .build();

    let meter_provider = MeterProviderBuilder::default()
        .with_resource(resource())
        .with_reader(reader)
        .build();

    global::set_meter_provider(meter_provider.clone());

    Ok(meter_provider)
}

fn init_tracer_provider(
    tracing_exporters: &TracingExporters,
) -> Result<SdkTracerProvider, anyhow::Error> {
    let otlp = tracing_exporters.otlp.as_ref().ok_or_else(|| {
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

    let trace_provider = SdkTracerProvider::builder()
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource())
        .with_batch_exporter(exporter)
        .build();

    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(trace_provider.clone());

    Ok(trace_provider)
}

/// Initialize tracing-subscriber and return TelemetryGuard for logging and opentelemetry-related termination processing
pub fn init_tracing_subscriber(config: &Config) -> Result<TelemetryGuard, anyhow::Error> {
    let tracer_provider = if let Some(exporters) = &config.telemetry.exporters {
        if let Some(tracing_exporters) = &exporters.tracing {
            init_tracer_provider(tracing_exporters)?
        } else {
            SdkTracerProvider::builder().build()
        }
    } else {
        SdkTracerProvider::builder().build()
    };
    let meter_provider = if let Some(exporters) = &config.telemetry.exporters {
        if let Some(metrics_exporters) = &exporters.metrics {
            init_meter_provider(metrics_exporters)?
        } else {
            SdkMeterProvider::builder().build()
        }
    } else {
        SdkMeterProvider::builder().build()
    };
    let env_filter = Logging::env_filter(&config.logging)?;
    let (logging_layer, logging_guard) = Logging::logging_layer(&config.logging)?;

    let tracer = tracer_provider.tracer("tracing-otel-subscriber");

    tracing_subscriber::registry()
        .with(logging_layer)
        .with(env_filter)
        .with(MetricsLayer::new(meter_provider.clone()))
        .with(OpenTelemetryLayer::new(tracer))
        .init();

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
