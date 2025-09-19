//! Runtime utilities
//!
//! This module is only used by the main binary and provides helper code
//! related to runtime configuration.

mod config;
mod endpoint;
mod filtering_exporter;
mod graphos;
mod introspection;
pub mod logging;
mod operation_source;
mod overrides;
mod schema_source;
mod schemas;
pub mod telemetry;

use std::path::Path;

pub use config::Config;
use figment::{
    Figment,
    providers::{Env, Format, Yaml},
};
pub use operation_source::{IdOrDefault, OperationSource};
pub use schema_source::SchemaSource;

/// Separator to use when drilling down into nested options in the env figment
const ENV_NESTED_SEPARATOR: &str = "__";

/// Read configuration from environment variables only (when no config file is provided)
#[allow(clippy::result_large_err)]
pub fn read_config_from_env() -> Result<Config, figment::Error> {
    Figment::new()
        .join(apollo_common_env())
        .join(Env::prefixed("APOLLO_MCP_").split(ENV_NESTED_SEPARATOR))
        .extract()
}

/// Read in a config from a YAML file, filling in any missing values from the environment
#[allow(clippy::result_large_err)]
pub fn read_config(yaml_path: impl AsRef<Path>) -> Result<Config, figment::Error> {
    Figment::new()
        .join(apollo_common_env())
        .join(Env::prefixed("APOLLO_MCP_").split(ENV_NESTED_SEPARATOR))
        .join(Yaml::file(yaml_path))
        .extract()
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

    #[test]
    fn it_merges_env_and_file_with_uplink_endpoints() {
        let config = "
            endpoint: http://from_file:4000/
        ";
        let saved_path = std::env::var("PATH").unwrap_or_default();
        let workspace = env!("CARGO_MANIFEST_DIR");

        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            jail.set_env("PATH", &saved_path);
            jail.set_env("INSTA_WORKSPACE_ROOT", workspace);
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env(
                "APOLLO_UPLINK_ENDPOINTS",
                "http://from_env:4000/,http://from_env2:4000/",
            );

            let config = read_config(path)?;

            insta::assert_debug_snapshot!(config, @r#"
            Config {
                custom_scalars: None,
                endpoint: Endpoint(
                    Url {
                        scheme: "http",
                        cannot_be_a_base: false,
                        username: "",
                        password: None,
                        host: Some(
                            Domain(
                                "from_file",
                            ),
                        ),
                        port: Some(
                            4000,
                        ),
                        path: "/",
                        query: None,
                        fragment: None,
                    },
                ),
                graphos: GraphOSConfig {
                    apollo_key: None,
                    apollo_graph_ref: None,
                    apollo_registry_url: None,
                    apollo_uplink_endpoints: [
                        Url {
                            scheme: "http",
                            cannot_be_a_base: false,
                            username: "",
                            password: None,
                            host: Some(
                                Domain(
                                    "from_env",
                                ),
                            ),
                            port: Some(
                                4000,
                            ),
                            path: "/",
                            query: None,
                            fragment: None,
                        },
                        Url {
                            scheme: "http",
                            cannot_be_a_base: false,
                            username: "",
                            password: None,
                            host: Some(
                                Domain(
                                    "from_env2",
                                ),
                            ),
                            port: Some(
                                4000,
                            ),
                            path: "/",
                            query: None,
                            fragment: None,
                        },
                    ],
                },
                headers: {},
                health_check: HealthCheckConfig {
                    enabled: false,
                    path: "/health",
                    readiness: ReadinessConfig {
                        interval: ReadinessIntervalConfig {
                            sampling: 5s,
                            unready: None,
                        },
                        allowed: 100,
                    },
                },
                introspection: Introspection {
                    execute: ExecuteConfig {
                        enabled: false,
                    },
                    introspect: IntrospectConfig {
                        enabled: false,
                        minify: false,
                    },
                    search: SearchConfig {
                        enabled: false,
                        index_memory_bytes: 50000000,
                        leaf_depth: 1,
                        minify: false,
                    },
                    validate: ValidateConfig {
                        enabled: false,
                    },
                },
                logging: Logging {
                    level: Level(
                        Info,
                    ),
                    path: None,
                    rotation: Hourly,
                },
                telemetry: Telemetry {
                    exporters: None,
                    service_name: None,
                    version: None,
                },
                operations: Infer,
                overrides: Overrides {
                    disable_type_description: false,
                    disable_schema_description: false,
                    enable_explorer: false,
                    mutation_mode: None,
                },
                schema: Uplink,
                transport: Stdio,
            }
            "#);
            Ok(())
        });
    }
}
