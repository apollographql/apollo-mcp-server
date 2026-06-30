use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct RhaiConfig {
    /// Directory containing Rhai scripts. The server loads `main.rhai` from this directory.
    #[serde(rename = "scripts")]
    pub scripts_dir: PathBuf,
}

impl Default for RhaiConfig {
    fn default() -> Self {
        Self {
            scripts_dir: PathBuf::from("rhai"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RhaiConfig;

    #[test]
    fn default_path_matches_existing_rhai_directory() {
        assert_eq!(
            RhaiConfig::default().scripts_dir,
            std::path::PathBuf::from("rhai")
        );
    }

    #[test]
    fn deserializes_custom_path() {
        let config = serde_yaml::from_str::<RhaiConfig>("scripts: /config/rhai\n")
            .expect("Rhai config should parse");

        assert_eq!(config.scripts_dir, std::path::PathBuf::from("/config/rhai"));
    }
}
