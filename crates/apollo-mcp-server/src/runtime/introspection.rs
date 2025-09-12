use schemars::JsonSchema;
use serde::Deserialize;
use std::path::Path;

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

    /// Validate configuration for checking operations before execution
    pub validate: ValidateConfig,
}

/// Execution-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ExecuteConfig {
    /// Enable introspection for execution
    pub enabled: bool,
    
    /// Additional hints to append to the execute tool description
    pub hints: Option<String>,
    
    /// Path to a file containing additional hints to append to the execute tool description.
    /// If both hints and hints_file are provided, hints_file takes precedence.
    /// Path is relative to the configuration file location.
    pub hints_file: Option<String>,
}

impl ExecuteConfig {
    /// Resolve hints from either inline hints or hints_file
    /// If hints_file is provided, it takes precedence over inline hints
    /// The file path is resolved relative to the config_dir if provided
    pub fn resolve_hints(&self, config_dir: Option<&Path>) -> Result<Option<String>, std::io::Error> {
        if let Some(hints_file) = &self.hints_file {
            let hints_path = if let Some(config_dir) = config_dir {
                config_dir.join(hints_file)
            } else {
                hints_file.into()
            };
            
            let hints_content = std::fs::read_to_string(&hints_path)
                .map_err(|e| {
                    tracing::warn!("Failed to read hints file '{}': {}", hints_path.display(), e);
                    e
                })?;
            
            tracing::info!("Loaded hints from file: {}", hints_path.display());
            Ok(Some(hints_content))
        } else {
            Ok(self.hints.clone())
        }
    }
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

    /// Minify search results
    pub minify: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            index_memory_bytes: 50_000_000,
            leaf_depth: 1,
            minify: false,
        }
    }
}

/// Validation tool configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ValidateConfig {
    /// Enable validation tool
    pub enabled: bool,
}

impl Introspection {
    /// Check if any introspection tools are enabled
    pub fn any_enabled(&self) -> bool {
        self.execute.enabled | self.introspect.enabled | self.search.enabled | self.validate.enabled
    }
}
