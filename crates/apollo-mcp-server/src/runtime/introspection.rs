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
