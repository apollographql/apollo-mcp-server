use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct PromptsConfig {
    /// Directory containing MCP prompt files. Every `.md` file in this directory
    /// is loaded as a prompt.
    pub path: PathBuf,
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("prompts"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PromptsConfig;

    #[test]
    fn default_path_matches_existing_prompts_directory() {
        assert_eq!(
            PromptsConfig::default().path,
            std::path::PathBuf::from("prompts")
        );
    }

    #[test]
    fn deserializes_custom_path() {
        let config = serde_yaml::from_str::<PromptsConfig>("path: /config/prompts\n")
            .expect("Prompts config should parse");

        assert_eq!(config.path, std::path::PathBuf::from("/config/prompts"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = serde_yaml::from_str::<PromptsConfig>("dir: /config/prompts\n")
            .expect_err("unknown field should be rejected")
            .to_string();

        assert!(err.contains("unknown field"), "unexpected error: {err}");
    }
}
