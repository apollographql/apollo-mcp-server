//! Logging config and utilities
//!
//! This module is only used by the main binary and provides logging config structures and setup
//! helper functions

mod defaults;
mod format_style;
mod log_rotation_kind;
mod parsers;

use crate::runtime::logging::format_style::FormatStyle;
use log_rotation_kind::LogRotationKind;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::RollingFileAppender;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, Layer as LayerTrait, Registry};

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

    #[serde(default)]
    pub format: FormatStyle,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: defaults::log_level(),
            path: None,
            rotation: defaults::default_rotation(),
            format: Default::default(),
        }
    }
}

type LoggingLayerResult = (
    Box<dyn LayerTrait<Registry> + Send + Sync>,
    Option<WorkerGuard>,
);

pub struct LoggingLayerBuilder {
    writer: Option<BoxMakeWriter>,
    worker_guard: Option<WorkerGuard>,
    ansi_enabled: bool,
}

impl LoggingLayerBuilder {
    pub fn new() -> Self {
        Self {
            writer: None,
            worker_guard: None,
            ansi_enabled: false,
        }
    }

    // This primarily to be used by unit tests to allow for dependency injection.
    // If no writer is provided when build() is called, one will be created using the logging config options.
    #[allow(dead_code)]
    pub fn with_writer<W>(mut self, mw: W) -> Self
    where
        W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
    {
        self.writer = Some(BoxMakeWriter::new(mw));
        self
    }

    // This primarily to be used by unit tests to allow for dependency injection
    #[allow(dead_code)]
    pub fn with_worker_guard(mut self, guard: WorkerGuard) -> Self {
        self.worker_guard = Some(guard);
        self
    }

    // This primarily to be used by unit tests to allow for dependency injection
    #[allow(dead_code)]
    pub fn with_ansi_enabled(mut self, enabled: bool) -> Self {
        self.ansi_enabled = enabled;
        self
    }

    pub fn build(mut self, logging: &Logging) -> Result<LoggingLayerResult, anyhow::Error> {
        if self.writer.is_none() {
            let (writer, guard, with_ansi) = self.build_writer(logging);
            self.writer = Some(writer);
            self.worker_guard = guard;
            self.ansi_enabled = with_ansi;
        }

        if let Some(writer) = self.writer {
            let layer = tracing_subscriber::fmt::layer();
            let formatted_layer = match logging.format {
                FormatStyle::Full => layer
                    .with_writer(writer)
                    .with_ansi(self.ansi_enabled)
                    .with_target(false)
                    .boxed(),
                FormatStyle::Compact => layer
                    .compact()
                    .with_writer(writer)
                    .with_ansi(self.ansi_enabled)
                    .with_target(false)
                    .boxed(),
                FormatStyle::Json => layer
                    .json()
                    .with_writer(writer)
                    .with_ansi(self.ansi_enabled)
                    .with_target(false)
                    .boxed(),
                FormatStyle::Pretty => layer
                    .pretty()
                    .with_writer(writer)
                    .with_ansi(self.ansi_enabled)
                    .with_target(false)
                    .boxed(),
            };

            Ok((formatted_layer, self.worker_guard))
        } else {
            Err(anyhow::Error::msg("No log writer set"))
        }
    }

    fn build_writer(&self, logging: &Logging) -> (BoxMakeWriter, Option<WorkerGuard>, bool) {
        macro_rules! log_error {
            () => {
                |e| eprintln!("Failed to setup logging: {e:?}")
            };
        }

        match logging.path.clone() {
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
        }
    }
}

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
    use crate::runtime::logging::format_style::FormatStyle;
    use crate::runtime::logging::log_rotation_kind::LogRotationKind;
    use crate::runtime::logging::{Logging, LoggingLayerBuilder};
    use regex::Regex;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing::subscriber;
    use tracing_core::Level;
    use tracing_subscriber::Registry;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt;

    struct TestBuffer(Arc<Mutex<Vec<u8>>>);
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for TestBuffer {
        type Writer = TestWriter;
        fn make_writer(&'a self) -> Self::Writer {
            TestWriter(self.0.clone())
        }
    }

    #[test]
    fn logs_in_json_format() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = TestBuffer(buf.clone());
        let logging_config = Logging {
            level: Level::INFO,
            path: None,
            rotation: LogRotationKind::Minutely,
            format: FormatStyle::Json,
        };

        let (logging_layer, _) = LoggingLayerBuilder::new()
            .with_writer(writer)
            .build(&logging_config)
            .unwrap();

        let sub = Registry::default().with(logging_layer);
        subscriber::with_default(sub, || {
            tracing::info!("hello!");

            let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
            assert!(out.contains("\"message\":\"hello!\""));
        });
    }

    #[test]
    fn logs_in_full_format() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = TestBuffer(buf.clone());
        let logging_config = Logging {
            level: Level::INFO,
            path: None,
            rotation: LogRotationKind::Minutely,
            format: FormatStyle::Full,
        };

        let (logging_layer, _) = LoggingLayerBuilder::new()
            .with_writer(writer)
            .with_ansi_enabled(false)
            .build(&logging_config)
            .unwrap();

        let sub = Registry::default().with(logging_layer);
        subscriber::with_default(sub, || {
            let expected_log_msg = "this is a log message";

            tracing::info!("{}", expected_log_msg);

            let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
            let pattern = format!(
                r"(?m)^\S+Z\s+INFO\s+{msg}$",
                msg = regex::escape(expected_log_msg)
            );
            let re = Regex::new(&pattern).unwrap();

            assert!(
                re.is_match(out.as_str()),
                "Log output did not match expected full format.\n  Expected pattern: {pattern}\n  Got: {out}"
            );
        });
    }

    #[test]
    fn logs_in_compact_format() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = TestBuffer(buf.clone());
        let logging_config = Logging {
            level: Level::INFO,
            path: None,
            rotation: LogRotationKind::Minutely,
            format: FormatStyle::Compact,
        };

        let (logging_layer, _) = LoggingLayerBuilder::new()
            .with_writer(writer)
            .build(&logging_config)
            .unwrap();

        let sub = Registry::default().with(logging_layer);
        subscriber::with_default(sub, || {
            let expected_log_msg = "this is a log message";

            tracing::info!("{}", expected_log_msg);

            let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
            let pattern = format!(
                r"(?m)^\S+Z\s+INFO\s+{msg}$",
                msg = regex::escape(expected_log_msg)
            );
            let re = Regex::new(&pattern).unwrap();
            println!("{}", out);

            assert!(
                re.is_match(out.as_str()),
                "Log output did not match expected full format.\n  Expected pattern: {pattern}\n  Got: {out}"
            );
        });
    }

    #[test]
    fn logs_in_pretty_format() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = TestBuffer(buf.clone());
        let logging_config = Logging {
            level: Level::INFO,
            path: None,
            rotation: LogRotationKind::Minutely,
            format: FormatStyle::Pretty,
        };

        let (logging_layer, _) = LoggingLayerBuilder::new()
            .with_writer(writer)
            .build(&logging_config)
            .unwrap();

        let sub = Registry::default().with(logging_layer);
        subscriber::with_default(sub, || {
            let expected_log_msg = "this is a log message";

            tracing::info!("{}", expected_log_msg);

            let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
            // Pattern matches a string like the following:
            //  2025-10-22T18:53:31.022676Z  INFO  Tool SearchUpcomingLaunches loaded with a character count of 545. Estimated tokens: 136
            //     at crates/apollo-mcp-server/src/operations/operation.rs:120
            //     in load_tool
            // Where the "in load_tool" portion is optional.
            let pattern = format!(
                r"(?m)^\s*\S+Z\s+INFO\s+{msg}\r?\n\s+at\s+.+\r?\n?(in)?.*$",
                msg = regex::escape(expected_log_msg)
            );
            let re = Regex::new(&pattern).unwrap();

            assert!(
                re.is_match(out.as_str()),
                "Log output did not match expected full format.\n  Expected pattern: {pattern}\n  Got: {out}"
            );
        });
    }
}
