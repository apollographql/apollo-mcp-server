//! Logging config and utilities
//!
//! This module is only used by the main binary and provides logging config structures and setup
//! helper functions

mod defaults;
mod log_rotation_kind;
mod parsers;

use log_rotation_kind::LogRotationKind;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::Level;
use tracing_appender::rolling::RollingFileAppender;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

/// Logging related options
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    /// The log level to use for tracing
    #[serde(
        default = "defaults::log_level",
        deserialize_with = "parsers::from_str"
    )]
    #[schemars(schema_with = "level")]
    pub level: Level,

    /// The output path to use for logging
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// Log file rotation period to use when log file path provided
    /// [default: Hourly]
    #[serde(default = "defaults::default_rotation")]
    pub rotation: LogRotationKind,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: defaults::log_level(),
            path: None,
            rotation: defaults::default_rotation(),
        }
    }
}

type LoggingLayerResult = (
    Layer<
        tracing_subscriber::Registry,
        tracing_subscriber::fmt::format::DefaultFields,
        tracing_subscriber::fmt::format::Format,
        BoxMakeWriter,
    >,
    Option<tracing_appender::non_blocking::WorkerGuard>,
);

impl Logging {
    pub fn env_filter(logging: &Logging) -> Result<EnvFilter, anyhow::Error> {
        let mut env_filter = EnvFilter::from_default_env().add_directive(logging.level.into());

        if logging.level == Level::INFO {
            env_filter = env_filter
                .add_directive("rmcp=warn".parse()?)
                .add_directive("tantivy=warn".parse()?);
        }
        Ok(env_filter)
    }

    pub fn logging_layer(logging: &Logging) -> Result<LoggingLayerResult, anyhow::Error> {
        macro_rules! log_error {
            () => {
                |e| eprintln!("Failed to setup logging: {e:?}")
            };
        }

        let (writer, guard, with_ansi) = match logging.path.clone() {
            Some(path) => std::fs::create_dir_all(&path)
                .map(|_| path)
                .inspect_err(log_error!())
                .ok()
                .and_then(|path| {
                    RollingFileAppender::builder()
                        .rotation(logging.rotation.clone().into())
                        .filename_prefix("apollo_mcp_server")
                        .filename_suffix("log")
                        .build(path)
                        .inspect_err(log_error!())
                        .ok()
                })
                .map(|appender| {
                    let (non_blocking_appender, guard) = tracing_appender::non_blocking(appender);
                    (
                        BoxMakeWriter::new(non_blocking_appender),
                        Some(guard),
                        false,
                    )
                })
                .unwrap_or_else(|| {
                    eprintln!("Log file setup failed - falling back to stderr");
                    (BoxMakeWriter::new(std::io::stderr), None, true)
                }),
            None => (BoxMakeWriter::new(std::io::stdout), None, true),
        };

        Ok((
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(with_ansi)
                .with_target(false),
            guard,
        ))
    }
}

fn level(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    /// Log level
    #[derive(JsonSchema)]
    #[schemars(rename_all = "lowercase")]
    // This is just an intermediate type to auto create schema information for,
    // so it is OK if it is never used
    #[allow(dead_code)]
    enum Level {
        Trace,
        Debug,
        Info,
        Warn,
        Error,
    }

    Level::json_schema(generator)
}
