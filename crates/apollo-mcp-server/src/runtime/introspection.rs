use schemars::JsonSchema;
use serde::Deserialize;

/// Introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Introspection {
    /// Execution configuration for introspection
    pub execute: ExecuteConfig,

    /// Introspect configuration for allowing clients to run introspection
    pub introspect: IntrospectConfig,

    /// Search tool configuration
    pub search: SearchConfig,

    /// Validate configuration for checking operations before execution
    pub validate: ValidateConfig,
}

/// Execution-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ExecuteConfig {
    /// Enable introspection for execution
    pub enabled: bool,
    /// Optional custom hint appended to the execute tool description
    pub hint: Option<String>,
}

/// Introspect-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IntrospectConfig {
    /// Enable introspection requests
    pub enabled: bool,

    /// Minify introspection results
    pub minify: bool,
    /// Optional custom hint appended to the introspect tool description
    pub hint: Option<String>,
}

/// Search tool configuration
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    /// Enable search tool
    pub enabled: bool,

    /// The amount of memory used for indexing (in bytes)
    pub index_memory_bytes: usize,

    /// The depth of subtype information to include from matching types
    /// (1 is just the matching type, 2 is the matching type plus the types it references, etc.
    /// Defaults to 1.)
    pub leaf_depth: usize,

    /// Minify search results
    pub minify: bool,
    /// Optional custom hint appended to the search tool description
    pub hint: Option<String>,
    /// Default number of results when the caller omits `limit`.
    pub default_limit: usize,
    /// Hard cap on the number of results the caller may request.
    pub max_limit: usize,
    /// Return-type flatten depth used to enrich each operation's index document.
    pub flatten_depth: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            index_memory_bytes: 50_000_000,
            leaf_depth: 1,
            minify: false,
            hint: None,
            default_limit: 10,
            max_limit: 50,
            flatten_depth: 2,
        }
    }
}

/// Validation tool configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ValidateConfig {
    /// Enable validation tool
    pub enabled: bool,
    /// Optional custom hint appended to the validate tool description
    pub hint: Option<String>,
}

impl Introspection {
    /// Check if any introspection tools are enabled
    pub fn any_enabled(&self) -> bool {
        self.execute.enabled | self.introspect.enabled | self.search.enabled | self.validate.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_config_defaults() {
        let c = SearchConfig::default();
        assert_eq!(c.default_limit, 10);
        assert_eq!(c.max_limit, 50);
        assert_eq!(c.flatten_depth, 2);
    }
}
