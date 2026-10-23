//! Configuration for asynchronous GraphQL execution through MCP Tasks.

use std::num::{NonZeroU64, NonZeroUsize};

use schemars::JsonSchema;
use serde::Deserialize;

/// Server-directed detachment of slow GraphQL calls (SEP-2663).
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// Enable the Tasks extension. Clients must also declare support on each request.
    pub enabled: bool,
    /// Time to wait for an ordinary result before returning a task handle; zero detaches immediately.
    pub detach_after_ms: u64,
    /// Task lifetime from detachment, also used as the terminal-result retention window.
    pub ttl_ms: NonZeroU64,
    /// Maximum number of running and retained tasks combined. At capacity, calls remain synchronous.
    pub max_tasks: NonZeroUsize,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            detach_after_ms: 1_000,
            ttl_ms: NonZeroU64::new(300_000).unwrap_or(NonZeroU64::MIN),
            max_tasks: NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_and_schema_agree() {
        let config: TasksConfig = serde_json::from_value(json!({})).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.detach_after_ms, 1000);
        assert_eq!(config.ttl_ms.get(), 300000);
        assert_eq!(config.max_tasks.get(), 1000);
        let schema = serde_json::to_value(schemars::schema_for!(TasksConfig)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for value in [
            json!({"ttl_ms":0}),
            json!({"max_tasks":0}),
            json!({"detach_after_ms":-1}),
            json!({"unknown":true}),
        ] {
            assert!(serde_json::from_value::<TasksConfig>(value.clone()).is_err());
            assert!(!validator.is_valid(&value));
        }
        assert!(validator.is_valid(&json!({"enabled":true,"detach_after_ms":0})));
    }
}
