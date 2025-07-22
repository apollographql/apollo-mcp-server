use crate::runtime::Config;
use crate::runtime::logging::log_rotation_kind::LogRotationKind;
use std::path::PathBuf;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::RollingFileAppender;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn setup_logging(config: &Config) -> Result<Option<WorkerGuard>, anyhow::Error> {
    let mut env_filter = EnvFilter::from_default_env().add_directive(config.logging.level.into());

    if config.logging.level == Level::INFO {
        env_filter = env_filter
            .add_directive("rmcp=warn".parse()?)
            .add_directive("tantivy=warn".parse()?);
    }

    if let Some(path) = &config.logging.path {
        setup_file_logging(path, env_filter, config.logging.rotation.clone())
    } else {
        setup_stderr_logging(env_filter)
    }
}

/// Sets up rolling file appender logging but falls back to stderr logging on failure
fn setup_file_logging(
    log_path: &PathBuf,
    env_filter: EnvFilter,
    log_rotation: LogRotationKind,
) -> Result<Option<WorkerGuard>, anyhow::Error> {
    match ensure_log_dir_exists(log_path.clone()) {
        Ok(..) => {}
        Err(_err) => {
            eprintln!("Could not build log path - falling back to stderr");
            return setup_stderr_logging(env_filter);
        }
    }

    let (non_blocking_writer, guard) = match RollingFileAppender::builder()
        .rotation(log_rotation.into())
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
