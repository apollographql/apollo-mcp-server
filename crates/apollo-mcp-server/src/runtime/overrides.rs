use apollo_mcp_server::operations::MutationMode;
use schemars::JsonSchema;
use serde::Deserialize;

/// Overridable flags
#[derive(Debug, Deserialize, Default, JsonSchema)]
pub struct Overrides {
    /// Disable type descriptions to save on context-window space
    pub disable_type_description: bool,

    /// Disable schema descriptions to save on context-window space
    pub disable_schema_description: bool,

    /// Expose the schema to the MCP client through `introspect` and `execute` tools
    pub enable_introspection: bool,

    /// Expose a tool that returns the URL to open a GraphQL operation in Apollo Explorer (requires APOLLO_GRAPH_REF)
    pub enable_explorer: bool,

    /// Set the mutation mode access level for the MCP server
    pub mutation_mode: MutationMode,
}
