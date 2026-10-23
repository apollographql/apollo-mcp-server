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

    fn is_terminal(self) -> bool {
        match self {
            Self::Stdout => std::io::stdout().is_terminal(),
            Self::Stderr => std::io::stderr().is_terminal(),
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
                        logging.ansi == AnsiMode::Always,
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
                console.make_writer(),
                None,
                should_use_ansi(logging.ansi, console.is_terminal(), ansi_env),
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

    fn env(
        no_color: Option<&'static str>,
        force_color: Option<&'static str>,
        clicolor_force: Option<&'static str>,
    ) -> AnsiEnv<'static> {
        AnsiEnv {
            no_color: no_color.map(OsStr::new),
            force_color: force_color.map(OsStr::new),
            clicolor_force: clicolor_force.map(OsStr::new),
        }
    }

    #[rstest]
    #[case::terminal(AnsiMode::Auto, true, env(None, None, None), true)]
    #[case::non_terminal(AnsiMode::Auto, false, env(None, None, None), false)]
    #[case::no_color(AnsiMode::Auto, true, env(Some("1"), None, None), false)]
    #[case::empty_no_color(AnsiMode::Auto, true, env(Some(""), None, None), true)]
    #[case::force_color(AnsiMode::Auto, false, env(None, Some("1"), None), true)]
    #[case::force_color_zero(AnsiMode::Auto, false, env(None, Some("0"), None), false)]
    #[case::empty_force_color(AnsiMode::Auto, false, env(None, Some(""), None), false)]
    #[case::clicolor_force(AnsiMode::Auto, false, env(None, None, Some("1")), true)]
    #[case::clicolor_force_zero(AnsiMode::Auto, false, env(None, None, Some("0")), false)]
    #[case::empty_clicolor_force(AnsiMode::Auto, false, env(None, None, Some("")), false)]
    #[case::no_color_over_force_color(
        AnsiMode::Auto,
        false,
        env(Some("1"), Some("1"), None),
        false
    )]
    #[case::no_color_over_clicolor_force(
        AnsiMode::Auto,
        false,
        env(Some("1"), None, Some("1")),
        false
    )]
    #[case::always_non_terminal(AnsiMode::Always, false, env(None, None, None), true)]
    #[case::always_over_no_color(AnsiMode::Always, true, env(Some("1"), None, None), true)]
    #[case::never_terminal(AnsiMode::Never, true, env(None, None, None), false)]
    #[case::never_over_force_color(AnsiMode::Never, false, env(None, Some("1"), Some("1")), false)]
    fn should_use_ansi_decision(
        #[case] mode: AnsiMode,
        #[case] is_terminal: bool,
        #[case] env: AnsiEnv<'static>,
        #[case] expected: bool,
    ) {
        assert_eq!(should_use_ansi(mode, is_terminal, env), expected);
    }

    #[rstest]
    #[case::auto("ansi: auto", AnsiMode::Auto)]
    #[case::always("ansi: always", AnsiMode::Always)]
    #[case::never("ansi: never", AnsiMode::Never)]
    #[case::default("{}", AnsiMode::Auto)]
    fn ansi_mode_deserializes(#[case] yaml: &str, #[case] expected: AnsiMode) {
        let logging: Logging = serde_yaml::from_str(yaml).expect("valid logging config");
        assert_eq!(logging.ansi, expected);
    }

    #[rstest]
    #[case::auto(AnsiMode::Auto, false)]
    #[case::always(AnsiMode::Always, true)]
    #[case::never(AnsiMode::Never, false)]
    fn log_file_uses_ansi_only_when_always(#[case] ansi: AnsiMode, #[case] expected: bool) {
        use tracing_subscriber::layer::SubscriberExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let logging = Logging {
            path: Some(dir.path().to_path_buf()),
            ansi,
            ..Logging::default()
        };
        let (layer, guard) =
            Logging::logging_layer(&logging, ConsoleStream::Stdout).expect("logging layer");
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            tracing::info!("hello");
        });
        drop(guard);

        let file = std::fs::read_dir(dir.path())
            .expect("log dir")
            .next()
            .expect("log file")
            .expect("dir entry");
        let contents = std::fs::read_to_string(file.path()).expect("log contents");
        assert!(contents.contains("hello"));
        assert_eq!(contents.contains('\x1b'), expected);
    }

    #[test]
    fn ansi_mode_rejects_unknown_value() {
        assert!(serde_yaml::from_str::<Logging>("ansi: sometimes").is_err());
    }
}
