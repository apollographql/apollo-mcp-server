use crate::runtime::Config;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Debug, Deserialize, JsonSchema, Clone)]
pub enum LogRotationKind {
    Minutely,
    Hourly,
    Daily,
    Never,
}

/// Logging related options
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Logging {
    /// The log level to use for tracing
    #[serde(
        default = "defaults::log_level",
        deserialize_with = "parsers::from_str"
    )]
    #[schemars(schema_with = "crate::runtime::schemas::level")]
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

mod defaults {
    use super::LogRotationKind;
    use tracing::Level;

    pub(crate) const fn log_level() -> Level {
        Level::INFO
    }

    pub(crate) const fn default_rotation() -> LogRotationKind {
        LogRotationKind::Hourly
    }
}

mod parsers {
    use std::{fmt::Display, marker::PhantomData, str::FromStr};

    use serde::Deserializer;

    pub(crate) fn from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr,
        <T as FromStr>::Err: Display,
    {
        struct FromStrVisitor<Inner> {
            _phantom: PhantomData<Inner>,
        }
        impl<Inner> serde::de::Visitor<'_> for FromStrVisitor<Inner>
        where
            Inner: FromStr,
            <Inner as FromStr>::Err: Display,
        {
            type Value = Inner;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Inner::from_str(v).map_err(|e| serde::de::Error::custom(e.to_string()))
            }
        }

        deserializer.deserialize_str(FromStrVisitor {
            _phantom: PhantomData,
        })
    }
}

pub fn setup_logging(config: &Config) -> Result<Option<WorkerGuard>, anyhow::Error> {
    let mut env_filter = EnvFilter::from_default_env().add_directive(config.logging.level.into());

    if config.logging.level == Level::INFO {
        env_filter = env_filter
            .add_directive("rmcp=warn".parse()?)
            .add_directive("tantivy=warn".parse()?);
    }

    if let Some(path) = &config.logging.path {
        setup_file_logging(path, env_filter, &config.logging.rotation)
    } else {
        setup_stderr_logging(env_filter)
    }
}

/// Sets up rolling file appender logging but falls back to stderr logging on failure
fn setup_file_logging(
    log_path: &PathBuf,
    env_filter: EnvFilter,
    log_rotation: &LogRotationKind,
) -> Result<Option<WorkerGuard>, anyhow::Error> {
    match ensure_log_dir_exists(log_path.clone()) {
        Ok(..) => {}
        Err(_err) => {
            eprintln!("Could not build log path - falling back to stderr");
            return setup_stderr_logging(env_filter);
        }
    }

    let (non_blocking_writer, guard) = match RollingFileAppender::builder()
        .rotation(map_rotation(log_rotation))
        .filename_prefix("apollo_mcp_server")
        .filename_suffix("log")
        .build(log_path)
    {
        Ok(appender) => tracing_appender::non_blocking(appender),
        Err(_error) => {
            eprintln!("Log file setup failed - falling back to stderr");
            return setup_stderr_logging(env_filter);
        }
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_writer)
                .with_ansi(false)
                .with_target(false),
        )
        .init();

    Ok(Some(guard))
}

/// Sets up stderr logging
fn setup_stderr_logging(env_filter: EnvFilter) -> Result<Option<WorkerGuard>, anyhow::Error> {
    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(true)
                .with_target(false),
        )
        .init();

    Ok(None)
}

/// Creates any missing directories in the log output path
fn ensure_log_dir_exists(dir: PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

fn map_rotation(log_rotation: &LogRotationKind) -> Rotation {
    match log_rotation {
        LogRotationKind::Minutely => Rotation::MINUTELY,
        LogRotationKind::Hourly => Rotation::HOURLY,
        LogRotationKind::Daily => Rotation::DAILY,
        LogRotationKind::Never => Rotation::NEVER,
    }
}
