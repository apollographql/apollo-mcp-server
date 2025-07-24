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
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::RollingFileAppender;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Logging related options
#[derive(Debug, Deserialize, JsonSchema)]
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

impl Logging {
    pub fn setup(&self) -> Result<Option<WorkerGuard>, anyhow::Error> {
        let mut env_filter = EnvFilter::from_default_env().add_directive(self.level.into());

        if self.level == Level::INFO {
            env_filter = env_filter
                .add_directive("rmcp=warn".parse()?)
                .add_directive("tantivy=warn".parse()?);
        }

        macro_rules! log_error {
            () => {
                |e| eprintln!("Error: {e:?}")
            };
            ($e:expr) => {
                |e| eprintln!("Error {}: {e:?}", $e)
            };
        }

        let (writer, guard, with_ansi) = self
            .path
            .clone()
            .and_then(|path| {
                std::fs::create_dir_all(&path)
                    .map(|_| path)
                    .inspect_err(log_error!())
                    .ok()
            })
            .and_then(|path| {
                RollingFileAppender::builder()
                    .rotation(self.rotation.clone().into())
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
            });

        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(with_ansi)
                    .with_target(false),
            )
            .init();

        Ok(guard)
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
