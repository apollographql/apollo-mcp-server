//! Logging config and utilities
//!
//! This module is only used by the main binary and provides logging config structures and setup
//! helper functions

mod defaults;
mod log_rotation_kind;
mod parsers;
mod trace_id_format;

use log_rotation_kind::LogRotationKind;
use schemars::JsonSchema;
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::IsTerminal;
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
        trace_id_format::TraceIdFormat,
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
        let no_color = std::env::var_os("NO_COLOR");
        let (writer, guard, with_ansi) = match logging.path.clone() {
            Some(path) => std::fs::create_dir_all(&path)
                .map(|_| path)
                .inspect_err(|e| eprintln!("Failed to setup logging: {e:?}"))
                .ok()
                .and_then(|path| {
                    RollingFileAppender::builder()
                        .rotation(logging.rotation.clone().into())
                        .filename_prefix("apollo_mcp_server")
                        .filename_suffix("log")
                        .build(path)
                        .inspect_err(|e| eprintln!("Failed to setup logging: {e:?}"))
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
                    (
                        BoxMakeWriter::new(std::io::stderr),
                        None,
                        should_use_ansi(std::io::stderr().is_terminal(), no_color.as_deref()),
                    )
                }),
            None => (
                BoxMakeWriter::new(std::io::stdout),
                None,
                should_use_ansi(std::io::stdout().is_terminal(), no_color.as_deref()),
            ),
        };

        let inner_format = tracing_subscriber::fmt::format::Format::default()
            .with_ansi(with_ansi)
            .with_target(false);

        Ok((
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(with_ansi)
                .event_format(trace_id_format::TraceIdFormat::new(inner_format)),
            guard,
        ))
    }
}

fn should_use_ansi(is_terminal: bool, no_color: Option<&OsStr>) -> bool {
    is_terminal && no_color.is_none_or(OsStr::is_empty)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn ansi_is_enabled_for_terminal_output() {
        assert!(should_use_ansi(true, None));
    }

    #[test]
    fn ansi_is_disabled_for_non_terminal_output() {
        assert!(!should_use_ansi(false, None));
    }

    #[test]
    fn ansi_is_disabled_when_no_color_is_set() {
        assert!(!should_use_ansi(true, Some(OsStr::new("1"))));
    }

    #[test]
    fn ansi_is_enabled_when_no_color_is_empty() {
        assert!(should_use_ansi(true, Some(OsStr::new(""))));
    }
}
