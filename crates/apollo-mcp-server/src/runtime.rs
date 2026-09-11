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
mod rhai;
mod schema_source;
mod schemas;
pub mod telemetry;

use std::mem::discriminant;
use std::path::Path;

pub use config::Config;
use figment::{
    Figment, Metadata, Profile, Provider,
    providers::{Env, Format, Yaml},
    value::{Dict, Map},
};
pub use operation_source::{IdOrDefault, OperationSource};
pub use schema_source::SchemaSource;

/// Separator to use when drilling down into nested options in the env figment
const ENV_NESTED_SEPARATOR: &str = "__";

/// Read configuration from environment variables only (when no config file is provided)
// `figment::Error` is large, but this runs once at startup, so boxing it buys nothing
#[expect(clippy::result_large_err)]
pub fn read_config_from_env() -> Result<Config, figment::Error> {
    env_figment().extract()
}

/// Read in a config from a YAML file, filling in any missing values from the environment.
///
/// Environment variable references using `${env.VAR_NAME}` syntax are expanded
/// before the YAML is parsed.
// `figment::Error` is large, but this runs once at startup, so boxing it buys nothing
#[expect(clippy::result_large_err)]
pub fn read_config(yaml_path: impl AsRef<Path>) -> Result<Config, figment::Error> {
    // Read and expand environment variables in the config content
    let content = std::fs::read_to_string(yaml_path.as_ref()).map_err(|e| {
        figment::Error::from(format!(
            "failed to read config file '{}': {}",
            yaml_path.as_ref().display(),
            e
        ))
    })?;

    let expanded = apollo_mcp_server::env_expansion::expand_yaml(&content)
        .map_err(|e| figment::Error::from(e.to_string()))?;

    let file = ConfigFile {
        path: yaml_path.as_ref(),
        expanded: &expanded,
    };

    env_figment()
        .join(&file)
        .extract()
        .map_err(|error| attribute_to_source(error, &file))
}

/// Figment provider for the config file, after `${env.VAR}` expansion. Wraps the YAML
/// provider so that errors name the file the operator wrote rather than an anonymous
/// source string.
struct ConfigFile<'a> {
    path: &'a Path,
    expanded: &'a str,
}

impl Provider for ConfigFile<'_> {
    fn metadata(&self) -> Metadata {
        Metadata::named(format!("config file '{}'", self.path.display()))
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        Yaml::string(self.expanded).data()
    }
}

/// Point a config error at the source that holds the offending value.
///
/// Figment tags a section assembled from several providers with whichever provider won
/// precedence, so without this an error in the config file is reported against the
/// environment as soon as any `APOLLO_MCP_` variable touches the same section.
fn attribute_to_source(mut error: figment::Error, file: &ConfigFile<'_>) -> figment::Error {
    if fails_without_the_environment(&error, file) {
        error.metadata = Some(file.metadata());
    }

    error
}

/// Whether the config file on its own fails the same way.
///
/// Figment reports an error against a key path only as precise as the deserializer that
/// failed, so for a section both sources filled in the path cannot say which one supplied
/// the offending value. Extracting the file by itself answers that directly: a failure
/// that survives without the environment belongs to the file.
fn fails_without_the_environment(error: &figment::Error, file: &ConfigFile<'_>) -> bool {
    let Err(without_env) = Figment::from(file).extract::<Config>() else {
        return false;
    };

    // The same kind of failure at the same key is the file's even when the details differ.
    // A file missing several required fields reports the first one, while the merged
    // config reports whichever of them the environment did not fill in.
    discriminant(&without_env.kind) == discriminant(&error.kind) && without_env.path == error.path
}

/// Figment for the `APOLLO_*` and `APOLLO_MCP_*` environment variables
fn env_figment() -> Figment {
    Figment::new()
        .join(apollo_common_env())
        .join(Env::prefixed("APOLLO_MCP_").split(ENV_NESTED_SEPARATOR))
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
    fn it_extracts_rhai_scripts_from_nested_env() {
        figment::Jail::expect_with(move |jail| {
            jail.set_env("APOLLO_MCP_RHAI__SCRIPTS", "/config/rhai");

            let config = super::read_config_from_env()?;

            assert_eq!(
                config.rhai.scripts_dir,
                std::path::PathBuf::from("/config/rhai")
            );
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
                caching: Caching {
                    ttl_ms: 300000,
                },
                cors: CorsConfig {
                    enabled: false,
                    origins: [],
                    match_origins: [],
                    allow_any_origin: false,
                    allow_credentials: false,
                    allow_methods: [
                        "GET",
                        "POST",
                        "DELETE",
                    ],
                    allow_headers: [
                        "content-type",
                        "mcp-protocol-version",
                        "mcp-session-id",
                        "traceparent",
                        "tracestate",
                        "baggage",
                    ],
                    expose_headers: [
                        "mcp-session-id",
                        "traceparent",
                        "tracestate",
                    ],
                    max_age: Some(
                        7200,
                    ),
                },
                server_info: ServerInfoConfig {
                    name: None,
                    version: None,
                    title: None,
                    website_url: None,
                    description: None,
                    icons: [],
                },
                instructions: None,
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
                forward_headers: [],
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
                rhai: RhaiConfig {
                    scripts_dir: "rhai",
                },
                introspection: Introspection {
                    execute: ExecuteConfig {
                        enabled: false,
                        hint: None,
                    },
                    introspect: IntrospectConfig {
                        enabled: false,
                        minify: false,
                        hint: None,
                    },
                    search: SearchConfig {
                        enabled: false,
                        index_memory_bytes: 50000000,
                        leaf_depth: 1,
                        minify: false,
                        hint: None,
                    },
                    validate: ValidateConfig {
                        enabled: false,
                        hint: None,
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
                    enable_output_schema: false,
                    enable_explorer: false,
                    mutation_mode: None,
                    descriptions: {},
                    annotations: {},
                    required_scopes: {},
                },
                schema: Uplink,
                transport: Stdio,
            }
            "#);
            Ok(())
        });
    }

    #[test]
    fn it_expands_env_vars_in_config() {
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                endpoint: ${env.TEST_EXPANDED_ENDPOINT}
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("TEST_EXPANDED_ENDPOINT", "https://expanded:4000/");

            let config = read_config(path)?;

            assert_eq!(config.endpoint.as_str(), "https://expanded:4000/");
            Ok(())
        });
    }

    #[test]
    fn it_prioritizes_apollo_mcp_env_over_expanded_vars() {
        // APOLLO_MCP_* should still override expanded ${env.VAR} values
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                endpoint: ${env.MY_ENDPOINT}
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("MY_ENDPOINT", "https://from_expansion:4000/");
            jail.set_env("APOLLO_MCP_ENDPOINT", "https://from_apollo_mcp:5000/");

            let config = read_config(path)?;

            // APOLLO_MCP_ENDPOINT wins
            assert_eq!(config.endpoint.as_str(), "https://from_apollo_mcp:5000/");
            Ok(())
        });
    }

    #[test]
    fn it_rejects_unknown_fields_in_yaml() {
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                auth:
                  servers:
                    - https://auth-server.com
                transport:
                  type: streamable_http
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;

            let result = read_config(path);
            assert!(result.is_err());

            let err = result.unwrap_err().to_string();
            assert!(err.contains("unknown field"));
            assert!(err.contains("auth"));
            Ok(())
        });
    }

    #[test]
    fn it_rejects_unknown_nested_fields_in_yaml() {
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                endpoint: http://localhost:4000/
                overrides:
                    unknown_flag: true
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;

            let result = read_config(path);
            assert!(result.is_err());

            let err = result.unwrap_err().to_string();
            assert!(err.contains("unknown field"));
            Ok(())
        });
    }

    #[test]
    fn it_names_the_config_file_in_yaml_errors() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                overrides:
                    unknown_flag: true
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;

            let err = read_config(path).unwrap_err().to_string();

            assert!(err.ends_with("in config file 'config.yaml'"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_config_file_when_env_touches_the_same_section() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                transport:
                    type: streamable_http
                    auth:
                        servers:
                            - https://auth.example.com
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_TRANSPORT__PORT", "5000");

            let err = read_config(path).unwrap_err().to_string();

            assert_eq!(
                err,
                "missing field `resource` for key \"default.transport\" in config file 'config.yaml'"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_config_file_for_unknown_fields_when_env_touches_the_same_section() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                transport:
                    type: streamable_http
                    bogus_field: 1
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_TRANSPORT__PORT", "5000");

            let err = read_config(path).unwrap_err().to_string();

            assert!(err.ends_with("in config file 'config.yaml'"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_config_file_when_its_root_is_not_a_mapping() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let path = "config.yaml";

            jail.create_file(path, "just-a-string\n")?;
            jail.set_env("APOLLO_MCP_TRANSPORT__PORT", "5000");

            let err = read_config(path).unwrap_err().to_string();

            assert_eq!(
                err,
                "invalid type: string \"just-a-string\", expected a map in config file 'config.yaml'"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_config_file_when_the_environment_fills_in_only_one_missing_field() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                transport:
                    type: streamable_http
                    auth: {}
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env(
                "APOLLO_MCP_TRANSPORT__AUTH__SERVERS",
                "[https://auth.example.com]",
            );

            let err = read_config(path).unwrap_err().to_string();

            assert_eq!(
                err,
                "missing field `resource` for key \"default.transport\" in config file 'config.yaml'"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_environment_when_it_overrides_a_valid_file_value() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                transport:
                    type: streamable_http
                    address: 127.0.0.1
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_TRANSPORT__ADDRESS", "not-an-ip-address");

            let err = read_config(path).unwrap_err().to_string();

            assert_eq!(
                err,
                "invalid IP address syntax for key \"TRANSPORT\" in `APOLLO_MCP_` environment variable(s)"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_environment_for_a_bad_item_in_a_list_it_supplies() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = "endpoint: http://localhost:4000/";
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_FORWARD_HEADERS", "[true]");

            let err = read_config(path).unwrap_err().to_string();

            assert!(
                err.ends_with("in `APOLLO_MCP_` environment variable(s)"),
                "{err}"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_environment_when_it_overrides_a_file_value() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                logging:
                    level: info
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_LOGGING__LEVEL", "nope");

            let err = read_config(path).unwrap_err().to_string();

            assert!(
                err.ends_with("in `APOLLO_MCP_` environment variable(s)"),
                "{err}"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_config_file_for_bad_values_when_env_touches_the_same_section() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                transport:
                    type: streamable_http
                    address: not-an-ip
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_TRANSPORT__PORT", "5000");

            let err = read_config(path).unwrap_err().to_string();

            assert_eq!(
                err,
                "invalid IP address syntax for key \"default.transport\" in config file 'config.yaml'"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_environment_for_unknown_env_fields() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = r#"
                endpoint: http://localhost:4000/
                transport:
                    type: streamable_http
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_TRANSPORT__BOGUS_FIELD", "1");

            let err = read_config(path).unwrap_err().to_string();

            assert!(
                err.ends_with("in `APOLLO_MCP_` environment variable(s)"),
                "{err}"
            );
            Ok(())
        });
    }

    #[test]
    fn it_blames_the_environment_for_sections_only_the_environment_sets() {
        figment::Jail::expect_with(move |jail| {
            jail.clear_env();
            let config = "endpoint: http://localhost:4000/";
            let path = "config.yaml";

            jail.create_file(path, config)?;
            jail.set_env("APOLLO_MCP_TRANSPORT__TYPE", "streamable_http");
            jail.set_env(
                "APOLLO_MCP_TRANSPORT__AUTH__SERVERS",
                "https://auth.example.com",
            );
            jail.set_env(
                "APOLLO_MCP_TRANSPORT__AUTH__RESOURCE",
                "https://mcp.example.com/mcp",
            );

            let err = read_config(path).unwrap_err().to_string();

            assert_eq!(
                err,
                "invalid type: found string \"https://auth.example.com\", expected a sequence \
                 for key \"TRANSPORT\" in `APOLLO_MCP_` environment variable(s)"
            );
            Ok(())
        });
    }

    #[test]
    fn it_parses_overrides_descriptions() {
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                endpoint: http://localhost:4000/
                overrides:
                    descriptions:
                        GetAlerts: "Fetch active weather alerts"
                        GetForecast: "Get the 7-day forecast"
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;

            let config = read_config(path)?;
            assert_eq!(config.overrides.descriptions.len(), 2);
            assert_eq!(
                config.overrides.descriptions.get("GetAlerts").unwrap(),
                "Fetch active weather alerts"
            );
            assert_eq!(
                config.overrides.descriptions.get("GetForecast").unwrap(),
                "Get the 7-day forecast"
            );
            Ok(())
        });
    }

    #[test]
    fn it_parses_overrides_annotations() {
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                endpoint: http://localhost:4000/
                overrides:
                    annotations:
                        GetAlerts:
                            read_only_hint: true
                            idempotent_hint: true
                        CreateUser:
                            destructive_hint: false
                            title: "Create a new user account"
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;

            let config = read_config(path)?;
            assert_eq!(config.overrides.annotations.len(), 2);

            let alerts = config.overrides.annotations.get("GetAlerts").unwrap();
            assert_eq!(alerts.read_only_hint, Some(true));
            assert_eq!(alerts.idempotent_hint, Some(true));
            assert_eq!(alerts.destructive_hint, None);

            let create_user = config.overrides.annotations.get("CreateUser").unwrap();
            assert_eq!(create_user.destructive_hint, Some(false));
            assert_eq!(
                create_user.title.as_deref(),
                Some("Create a new user account")
            );
            Ok(())
        });
    }

    #[test]
    fn caching_environment_overrides_yaml() {
        figment::Jail::expect_with(move |jail| {
            let config = r#"
                endpoint: http://localhost:4000/
                caching:
                    ttl_ms: 60000
            "#;
            let path = "config.yaml";

            jail.create_file(path, config)?;

            let config = read_config(path)?;
            assert_eq!(config.caching.ttl_ms, 60_000);
            jail.set_env("APOLLO_MCP_CACHING__TTL_MS", "0");
            assert_eq!(read_config(path)?.caching.ttl_ms, 0);
            Ok(())
        });
    }
}
