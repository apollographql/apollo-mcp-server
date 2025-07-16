use schemars::JsonSchema;
use serde::Deserialize;

/// Introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Introspection {
    /// Execution configuration for introspection
    pub execute: ExecuteConfig,

    /// Introspect configuration for allowing clients to run introspection
    pub introspect: IntrospectConfig,

    /// Search tool configuration
    pub search: SearchConfig,
}

/// Execution-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ExecuteConfig {
    /// Enable introspection for execution
    pub enabled: bool,
}

/// Introspect-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct IntrospectConfig {
    /// Enable introspection requests
    pub enabled: bool,

    /// Minify introspection results
    pub minify: bool,
}

/// Search tool configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SearchConfig {
    /// Enable search tool
    pub enabled: bool,

    /// Minify search results
    pub minify: bool,
}

impl Introspection {
    /// Check if any introspection tools are enabled
    pub fn any_enabled(&self) -> bool {
        self.execute.enabled | self.introspect.enabled | self.search.enabled
    }
}
