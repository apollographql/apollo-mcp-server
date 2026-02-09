use std::path::PathBuf;

use apollo_mcp_server::{
    cors::CorsConfig, headers::ForwardHeaders, health::HealthCheckConfig, server::Transport,
};
use reqwest::header::HeaderMap;
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

use apollo_mcp_server::server_info::ServerInfoConfig;

use super::{
    OperationSource, SchemaSource, endpoint::Endpoint, graphos::GraphOSConfig,
    introspection::Introspection, logging::Logging, overrides::Overrides, telemetry::Telemetry,
};

/// Configuration for the MCP server
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// CORS configuration
    pub cors: CorsConfig,

    /// Server metadata configuration
    #[serde(default)]
    pub server_info: ServerInfoConfig,

    /// Path to a custom scalar map
    pub custom_scalars: Option<PathBuf>,

    /// The target GraphQL endpoint
    #[schemars(schema_with = "Url::json_schema")]
    pub endpoint: Endpoint,

    /// Apollo-specific credential overrides
    pub graphos: GraphOSConfig,

    /// List of hard-coded headers to include in all GraphQL requests
    #[serde(deserialize_with = "parsers::map_from_str")]
    #[schemars(schema_with = "super::schemas::header_map")]
    pub headers: HeaderMap,

    /// List of header names to forward from MCP client requests to GraphQL requests
    #[serde(default)]
    pub forward_headers: ForwardHeaders,

    /// Health check configuration
    #[serde(default)]
    pub health_check: HealthCheckConfig,

    /// Introspection configuration
    pub introspection: Introspection,

    /// Logging configuration
    pub logging: Logging,

    /// Telemetry configuration
    pub telemetry: Telemetry,

    /// Operations
    pub operations: OperationSource,

    /// Overrides for server behaviour
    pub overrides: Overrides,

    /// The schema to load for operations
    pub schema: SchemaSource,

    /// The type of server transport to use
    pub transport: Transport,
}

mod parsers {
    use std::str::FromStr;

    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    use serde::Deserializer;

    pub(super) fn map_from_str<'de, D>(deserializer: D) -> Result<HeaderMap, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MapFromStrVisitor;
        impl<'de> serde::de::Visitor<'de> for MapFromStrVisitor {
            type Value = HeaderMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a map of header string keys and values")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut parsed = HeaderMap::with_capacity(map.size_hint().unwrap_or(0));

                // While there are entries remaining in the input, add them
                // into our map.
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    let key = HeaderName::from_str(&key)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))?;
                    let value = HeaderValue::from_str(&value)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))?;

                    parsed.insert(key, value);
                }

                Ok(parsed)
            }
        }

        deserializer.deserialize_map(MapFromStrVisitor)
    }
}

#[cfg(test)]
mod test {
    use super::Config;

    #[test]
    fn it_parses_a_minimal_config() {
        serde_json::from_str::<Config>("{}").unwrap();
    }

    #[test]
    fn it_contains_no_keys_with_double_underscore() {
        // The env functionality of the config expansion uses __ as a split key
        // when determining nested fields of any of the fields of the Config.
        // This test ensures that a field name isn't added that can no longer be
        // configured using the env extractor.
        //
        // See [runtime::read_config]
        //
        // TODO: This is a quick hack since traversing the nested (untyped) schema
        // object is probably overkill.
        let schema = schemars::schema_for!(Config).to_value().to_string();

        assert!(!schema.contains("__"))
    }

    #[test]
    fn it_rejects_unknown_top_level_fields() {
        let result = serde_json::from_str::<Config>(r#"{"auth": {}}"#);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown field"));
        assert!(err.contains("auth"));
    }

    #[test]
    fn it_rejects_unknown_nested_fields() {
        let result = serde_json::from_str::<Config>(
            r#"{"introspection": {"execute": {"unknown_option": true}}}"#,
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn it_rejects_auth_at_top_level() {
        // This is the exact scenario from the issue: auth placed at top level
        // instead of nested under transport
        let yaml = r#"
            auth:
              servers:
                - https://auth-server.com
            transport:
              type: streamable_http
        "#;
        let result = serde_yaml::from_str::<Config>(yaml);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown field"));
    }
}
