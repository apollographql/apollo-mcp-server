use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_sdk::{
    Resource,
    metrics::{MeterProviderBuilder, PeriodicReader, SdkMeterProvider},
    trace::{RandomIdGenerator, SdkTracerProvider},
};
use opentelemetry_semantic_conventions::{
    SCHEMA_URL,
    attribute::{DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_VERSION},
};
use tracing_opentelemetry::{MetricsLayer, OpenTelemetryLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use super::{Config, logging::Logging};

/// Create a Resource that captures information about the entity for which telemetry is recorded.
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

/// Construct MeterProvider for MetricsLayer
fn init_meter_provider() -> Result<SdkMeterProvider, anyhow::Error> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_temporality(opentelemetry_sdk::metrics::Temporality::default())
        .build()?;

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

/// Construct TracerProvider for OpenTelemetryLayer
fn init_tracer_provider() -> Result<SdkTracerProvider, anyhow::Error> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()?;

    let trace_provider = SdkTracerProvider::builder()
        // TODO: Should this use session information to group spans to a request?
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource())
        .with_batch_exporter(exporter)
        .build();

    global::set_tracer_provider(trace_provider.clone());

    Ok(trace_provider)
}

/// Initialize tracing-subscriber and return OtelGuard for opentelemetry-related termination processing
pub fn init_tracing_subscriber(config: &Config) -> Result<TelemetryGuard, anyhow::Error> {
    let tracer_provider = init_tracer_provider()?;
    let meter_provider = init_meter_provider()?;
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
