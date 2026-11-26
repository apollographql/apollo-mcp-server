use std::{collections::BTreeMap, path::PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct AppsConfig {
    /// Directory containing MCP Apps. Each subdirectory holding an
    /// `.application-manifest.json` file is loaded as an app.
    pub path: PathBuf,
    /// App tools to expose on connections without the `app` parameter, as a map of
    /// app name to manifest tool names.
    pub expose_without_app_param: BTreeMap<String, Vec<String>>,
}

impl Default for AppsConfig {
    fn default() -> Self {
        Self {
            expose_without_app_param: Default::default(),
            path: PathBuf::from("apps"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppsConfig;

    #[test]
    fn default_path_matches_existing_apps_directory() {
        assert_eq!(AppsConfig::default().path, std::path::PathBuf::from("apps"));
    }

    #[test]
    fn deserializes_custom_path() {
        let config = yaml_serde::from_str::<AppsConfig>("path: /config/apps\n")
            .expect("Apps config should parse");

        assert_eq!(config.path, std::path::PathBuf::from("/config/apps"));
    }

    #[test]
    fn expose_without_app_param_is_empty_unless_set() {
        assert!(AppsConfig::default().expose_without_app_param.is_empty());
    }

    #[test]
    fn deserializes_expose_without_app_param() {
        let config = yaml_serde::from_str::<AppsConfig>(
            "expose_without_app_param:\n  analytics: [usage_chart]\n",
        )
        .expect("Apps config should parse");

        assert_eq!(
            config.expose_without_app_param,
            std::collections::BTreeMap::from([(
                "analytics".to_owned(),
                vec!["usage_chart".to_owned()]
            )])
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = yaml_serde::from_str::<AppsConfig>("dir: /config/apps\n")
            .expect_err("unknown field should be rejected")
            .to_string();

        assert!(err.contains("unknown field"), "unexpected error: {err}");
    }
}
