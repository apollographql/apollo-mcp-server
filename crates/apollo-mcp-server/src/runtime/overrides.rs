use apollo_mcp_server::operations::MutationMode;
use schemars::JsonSchema;
use serde::Deserialize;

/// Overridable flags
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct Overrides {
    /// Disable type descriptions to save on context-window space
    pub disable_type_description: bool,

    /// Disable schema descriptions to save on context-window space
    pub disable_schema_description: bool,

    /// Expose a tool that returns the URL to open a GraphQL operation in Apollo Explorer (requires APOLLO_GRAPH_REF)
    pub enable_explorer: bool,

    /// Set the mutation mode access level for the MCP server
    pub mutation_mode: MutationMode,
}
