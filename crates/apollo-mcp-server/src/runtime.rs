//! Runtime utilites
//!
//! This module is only used by the main binary and provides helper code
//! related to runtime configuration.

mod config;
mod endpoint;
mod graphos;
mod introspection;
mod logging;
mod operation_source;
mod overrides;
mod schema_source;
mod schemas;

use std::path::{Path, PathBuf};

pub use config::Config;
use figment::{
    Figment,
    providers::{Env, Format, Yaml},
};
pub use operation_source::{IdOrDefault, OperationSource};
pub use schema_source::SchemaSource;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Separator to use when drilling down into nested options in the env figment
const ENV_NESTED_SEPARATOR: &str = "__";

/// Read in a config from a YAML file, filling in any missing values from the environment
#[allow(clippy::result_large_err)]
pub fn read_config(yaml_path: impl AsRef<Path>) -> Result<Config, figment::Error> {
    Figment::new()
        .join(apollo_common_env())
        .join(Env::prefixed("APOLLO_MCP_").split(ENV_NESTED_SEPARATOR))
        .join(Yaml::file(yaml_path))
        .extract()
}

/// Sets up either file logging or stderr logging depending on provided configuration options
pub fn setup_logging(config: &Config) -> Result<Option<WorkerGuard>, anyhow::Error> {
    let mut env_filter = EnvFilter::from_default_env().add_directive(config.logging.level.into());

    if config.logging.level == Level::INFO {
        env_filter = env_filter
            .add_directive("rmcp=warn".parse()?)
            .add_directive("tantivy=warn".parse()?);
    }

    if let Some(path) = &config.logging.path {
        setup_file_logging(path, env_filter)
    } else {
        setup_stderr_logging(env_filter)
    }
}

/// Sets up rolling file appender logging but falls back to stderr logging on failure
fn setup_file_logging(
    log_path: &PathBuf,
    env_filter: EnvFilter,
) -> Result<Option<WorkerGuard>, anyhow::Error> {
    match ensure_log_dir_exists(log_path.clone()) {
        Ok(..) => {}
        Err(_err) => {
            eprintln!("Failed to build log path - falling back to stderr");
            return setup_stderr_logging(env_filter);
        }
    }

    let (non_blocking_writer, guard) = match RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix("apollo_mcp_server")
        .filename_suffix("log")
        .build(log_path)
    {
        Ok(appender) => tracing_appender::non_blocking(appender),
        Err(_error) => {
            eprintln!("Failed to build log file - falling back to stderr");
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

/// Figment provider that handles mapping common Apollo environment variables into
/// the nested structure needed by the config
fn apollo_common_env() -> Env {
    Env::prefixed("APOLLO_")
        .only(&["graph_ref", "key", "uplink_endpoints"])
        .map(|key| match key.to_string().to_lowercase().as_str() {
            "graph_ref" => "GRAPHOS:APOLLO_GRAPH_REF".into(),
            "key" => "GRAPHOS:APOLLO_KEY".into(),
            "uplink_endpoints" => "GRAPHOS:APOLLO_UPLINK_ENDPOINTS".into(),

            // This case should never happen, so we just pass through this case as is
            other => other.to_string().into(),
        })
        .split(":")
}

#[cfg(test)]
mod test {
    use super::read_config;

    #[test]
    fn it_prioritizes_env_vars() {
        let config = r#"
            endpoint: http://from_file:4000
        "#;

        figment::Jail::expect_with(move |jail| {
            let path = "config.yaml";
            let endpoint = "https://from_env:4000/";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_ENDPOINT", endpoint);

            let config = read_config(path)?;

            assert_eq!(config.endpoint.as_str(), endpoint);
            Ok(())
        });
    }

    #[test]
    fn it_extracts_nested_env() {
        let config = r#"
            overrides:
                disable_type_description: false
        "#;

        figment::Jail::expect_with(move |jail| {
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_OVERRIDES__DISABLE_TYPE_DESCRIPTION", "true");

            let config = read_config(path)?;

            assert!(config.overrides.disable_type_description);
            Ok(())
        });
    }

    #[test]
    fn it_merges_env_and_file() {
        let config = "
            endpoint: http://from_file:4000/
        ";

        figment::Jail::expect_with(move |jail| {
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_INTROSPECTION__EXECUTE__ENABLED", "true");

            let config = read_config(path)?;

            assert_eq!(config.endpoint.as_str(), "http://from_file:4000/");
            assert!(config.introspection.execute.enabled);
            Ok(())
        });
    }
}
