//! Logging config and utilities
//!
//! This module is only used by the main binary and provides logging config structures and setup
//! helper functions

mod defaults;
mod log_rotation_kind;
mod parsers;
mod trace_id_format;

use apollo_mcp_server::server::Transport;
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

/// The standard stream that receives log output when no log file path is configured
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleStream {
    Stdout,
    /// Required by the stdio transport, which reserves stdout for MCP messages
    Stderr,
}

impl ConsoleStream {
    /// The console stream that is safe to log to for the given transport
    pub fn for_transport(transport: &Transport) -> Self {
        match transport {
            Transport::Stdio {} => Self::Stderr,
            Transport::StreamableHttp { .. } => Self::Stdout,
        }
    }

    fn make_writer(self) -> BoxMakeWriter {
        match self {
            Self::Stdout => BoxMakeWriter::new(std::io::stdout),
            Self::Stderr => BoxMakeWriter::new(std::io::stderr),
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

    pub fn logging_layer(
        logging: &Logging,
        console: ConsoleStream,
    ) -> Result<LoggingLayerResult, anyhow::Error> {
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
                    (BoxMakeWriter::new(std::io::stderr), None, true)
                }),
            None => (console.make_writer(), None, true),
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
    use rstest::rstest;

    #[rstest]
    #[case::stdio("type: stdio", ConsoleStream::Stderr)]
    #[case::streamable_http("type: streamable_http", ConsoleStream::Stdout)]
    fn console_stream_for_transport(#[case] yaml: &str, #[case] expected: ConsoleStream) {
        let transport: Transport = serde_yaml::from_str(yaml).expect("valid transport");
        assert_eq!(ConsoleStream::for_transport(&transport), expected);
    }

    #[rstest]
    #[case::stdout(ConsoleStream::Stdout)]
    #[case::stderr(ConsoleStream::Stderr)]
    fn logging_layer_without_path_has_no_file_guard(#[case] console: ConsoleStream) {
        let (_layer, guard) =
            Logging::logging_layer(&Logging::default(), console).expect("logging layer");
        assert!(guard.is_none());
    }
}
