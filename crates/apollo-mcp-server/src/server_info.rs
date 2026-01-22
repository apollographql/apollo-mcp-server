use schemars::JsonSchema;
use serde::Deserialize;

/// Server metadata configuration
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
    pub fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| "Apollo MCP Server".to_string())
    }

    pub fn version(&self) -> String {
        self.version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
    }

    pub fn title(&self) -> Option<String> {
        self.title
            .clone()
            .or_else(|| Some("Apollo MCP Server".to_string()))
    }

    pub fn website_url(&self) -> Option<String> {
        self.website_url
            .clone()
            .or_else(|| Some("https://www.apollographql.com/docs/apollo-mcp-server".to_string()))
    }
}
