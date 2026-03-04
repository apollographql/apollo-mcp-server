use std::collections::HashMap;

use apollo_mcp_server::operations::MutationMode;
use schemars::JsonSchema;
use serde::Deserialize;

/// Overridable flags
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Overrides {
    /// Disable type descriptions to save on context-window space
    pub disable_type_description: bool,

    /// Disable schema descriptions to save on context-window space
    pub disable_schema_description: bool,

    /// Enable output schema generation for tools (adds token overhead but helps LLMs understand response structure)
    pub enable_output_schema: bool,

    /// Expose a tool that returns the URL to open a GraphQL operation in Apollo Explorer (requires APOLLO_GRAPH_REF)
    pub enable_explorer: bool,

    /// Set the mutation mode access level for the MCP server
    pub mutation_mode: MutationMode,

    /// Optional map from operation name to tool description. When provided,
    /// these descriptions override the auto-generated tool descriptions for
    /// the matching operations, regardless of the operation source.
    pub descriptions: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_with_descriptions_parses() {
        let json = serde_json::json!({
            "descriptions": {
                "GetAlerts": "Fetch active weather alerts",
                "GetForecast": "Get the 7-day forecast"
            }
        });

        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert_eq!(
            overrides.descriptions,
            HashMap::from([
                (
                    "GetAlerts".to_string(),
                    "Fetch active weather alerts".to_string()
                ),
                (
                    "GetForecast".to_string(),
                    "Get the 7-day forecast".to_string()
                ),
            ])
        );
    }

    #[test]
    fn overrides_without_descriptions_defaults_to_empty() {
        let json = serde_json::json!({});
        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert!(overrides.descriptions.is_empty());
    }
}
