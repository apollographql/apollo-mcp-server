use schemars::JsonSchema;
use serde::Deserialize;

/// Server metadata configuration returned in the MCP initialize response.
/// All fields are optional and fall back to defaults if not provided.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ServerInfoConfig {
    /// The name of the MCP server implementation
    pub name: Option<String>,

    /// The version of the MCP server implementation
    pub version: Option<String>,

    /// Human-readable title for the server
    pub title: Option<String>,

    /// URL to the server's website or documentation
    pub website_url: Option<String>,
}

impl ServerInfoConfig {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("Apollo MCP Server")
    }

    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"))
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref().or(Some("Apollo MCP Server"))
    }

    pub fn website_url(&self) -> Option<&str> {
        self.website_url
            .as_deref()
            .or(Some("https://www.apollographql.com/docs/apollo-mcp-server"))
    }
}
