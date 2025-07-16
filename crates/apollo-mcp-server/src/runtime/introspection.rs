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
}

/// Search tool configuration
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SearchConfig {
    /// Enable search tool
    pub enabled: bool,

    /// The amount of memory used for indexing (in bytes)
    pub index_memory_bytes: usize,

    /// The depth of subtype information to include from matching types
    /// (1 is just the matching type, 2 is the matching type plus the types it references, etc.
    /// Defaults to 1.)
    pub leaf_depth: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            index_memory_bytes: 50_000_000,
            leaf_depth: 1,
        }
    }
}

impl Introspection {
    /// Check if any introspection tools are enabled
    pub fn any_enabled(&self) -> bool {
        self.execute.enabled | self.introspect.enabled | self.search.enabled
    }
}
