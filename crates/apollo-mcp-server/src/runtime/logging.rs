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

/// ANSI styling mode for log output.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnsiMode {
    /// Enable ANSI styling only when the output stream is a terminal, unless
    /// overridden by `NO_COLOR`, `FORCE_COLOR`, or `CLICOLOR_FORCE`.
    #[default]
    Auto,
    /// Always enable ANSI styling, regardless of terminal detection or
    /// environment variables.
    Always,
    /// Never enable ANSI styling, regardless of terminal detection or
    /// environment variables.
    Never,
}

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

    /// ANSI styling mode for log output [default: auto]
    #[serde(default)]
    pub ansi: AnsiMode,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: defaults::log_level(),
            path: None,
            rotation: defaults::default_rotation(),
            ansi: AnsiMode::default(),
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
        let force_color = std::env::var_os("FORCE_COLOR");
        let clicolor_force = std::env::var_os("CLICOLOR_FORCE");
        let ansi_env = AnsiEnv {
            no_color: no_color.as_deref(),
            force_color: force_color.as_deref(),
            clicolor_force: clicolor_force.as_deref(),
        };
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
                        should_use_ansi(logging.ansi, std::io::stderr().is_terminal(), ansi_env),
                    )
                }),
            None => (
                BoxMakeWriter::new(std::io::stdout),
                None,
                should_use_ansi(logging.ansi, std::io::stdout().is_terminal(), ansi_env),
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

/// Environment variables consulted when `logging.ansi` is `auto`.
#[derive(Clone, Copy, Debug, Default)]
struct AnsiEnv<'a> {
    no_color: Option<&'a OsStr>,
    force_color: Option<&'a OsStr>,
    clicolor_force: Option<&'a OsStr>,
}

fn should_use_ansi(mode: AnsiMode, is_terminal: bool, env: AnsiEnv<'_>) -> bool {
    match mode {
        AnsiMode::Always => true,
        AnsiMode::Never => false,
        AnsiMode::Auto => {
            if env.no_color.is_some_and(|value| !value.is_empty()) {
                false
            } else if is_force_flag_set(env.force_color) || is_force_flag_set(env.clicolor_force) {
                true
            } else {
                is_terminal
            }
        }
    }
}

/// True when a "force color" variable (`FORCE_COLOR`, `CLICOLOR_FORCE`) is
/// present with a value other than empty or `"0"`, both of which mean "not
/// forcing" by widespread convention.
fn is_force_flag_set(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != OsStr::new("0"))
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

    fn env(no_color: Option<&'static str>) -> AnsiEnv<'static> {
        AnsiEnv {
            no_color: no_color.map(OsStr::new),
            ..AnsiEnv::default()
        }
    }

    #[test]
    fn ansi_is_enabled_for_terminal_output() {
        assert!(should_use_ansi(AnsiMode::Auto, true, env(None)));
    }

    #[test]
    fn ansi_is_disabled_for_non_terminal_output() {
        assert!(!should_use_ansi(AnsiMode::Auto, false, env(None)));
    }

    #[test]
    fn ansi_is_disabled_when_no_color_is_set() {
        assert!(!should_use_ansi(AnsiMode::Auto, true, env(Some("1"))));
    }

    #[test]
    fn ansi_is_enabled_when_no_color_is_empty() {
        assert!(should_use_ansi(AnsiMode::Auto, true, env(Some(""))));
    }

    #[test]
    fn ansi_is_enabled_for_non_terminal_when_force_color_is_set() {
        let env = AnsiEnv {
            force_color: Some(OsStr::new("1")),
            ..AnsiEnv::default()
        };
        assert!(should_use_ansi(AnsiMode::Auto, false, env));
    }

    #[test]
    fn ansi_is_enabled_for_non_terminal_when_clicolor_force_is_set() {
        let env = AnsiEnv {
            clicolor_force: Some(OsStr::new("1")),
            ..AnsiEnv::default()
        };
        assert!(should_use_ansi(AnsiMode::Auto, false, env));
    }

    #[test]
    fn force_color_of_zero_does_not_force_ansi_on() {
        let env = AnsiEnv {
            force_color: Some(OsStr::new("0")),
            ..AnsiEnv::default()
        };
        assert!(!should_use_ansi(AnsiMode::Auto, false, env));
    }

    #[test]
    fn force_color_of_empty_string_does_not_force_ansi_on() {
        let env = AnsiEnv {
            force_color: Some(OsStr::new("")),
            ..AnsiEnv::default()
        };
        assert!(!should_use_ansi(AnsiMode::Auto, false, env));
    }

    #[test]
    fn no_color_takes_precedence_over_force_color() {
        let env = AnsiEnv {
            no_color: Some(OsStr::new("1")),
            force_color: Some(OsStr::new("1")),
            ..AnsiEnv::default()
        };
        assert!(!should_use_ansi(AnsiMode::Auto, false, env));
    }

    #[test]
    fn always_mode_ignores_terminal_detection_and_no_color() {
        assert!(should_use_ansi(AnsiMode::Always, false, env(Some("1"))));
    }

    #[test]
    fn never_mode_ignores_terminal_detection_and_force_color() {
        let env = AnsiEnv {
            force_color: Some(OsStr::new("1")),
            ..AnsiEnv::default()
        };
        assert!(!should_use_ansi(AnsiMode::Never, true, env));
    }
}
