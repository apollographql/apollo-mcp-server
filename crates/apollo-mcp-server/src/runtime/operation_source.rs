use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

/// Source for loaded operations
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSource {
    /// Load operations from a GraphOS collection
    Collection {
        #[schemars(with = "String")]
        id: IdOrDefault,
    },

    /// Infer where to load operations based on other configuration options.
    ///
    /// Note: This setting tries to load the operations from introspection, if enabled
    /// or from the default operation collection when APOLLO_GRAPH_REF is set.
    #[default]
    Infer,

    /// Load operations by introspecting the schema
    ///
    /// Note: Requires introspection to be enabled
    Introspect,

    /// Load operations from local GraphQL files / folders
    Local { paths: Vec<PathBuf> },

    /// Load operations from a persisted queries manifest file
    Manifest { path: PathBuf },

    /// Load operations from uplink manifest
    Uplink,
}

/// Either a custom ID or the default variant
#[derive(Debug, PartialEq, Eq)]
pub enum IdOrDefault {
    /// The default tools for the variant (requires APOLLO_KEY)
    Default,

    /// The specific collection ID
    Id(String),
}

impl<'de> Deserialize<'de> for IdOrDefault {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct IdOrDefaultVisitor;
        impl serde::de::Visitor<'_> for IdOrDefaultVisitor {
            type Value = IdOrDefault;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or 'default'")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let variant = if v.to_lowercase() == "default" {
                    IdOrDefault::Default
                } else {
                    IdOrDefault::Id(v.to_string())
                };

                Ok(variant)
            }
        }

        deserializer.deserialize_str(IdOrDefaultVisitor)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn id_parses() {
        let id = "something";

        let actual: IdOrDefault =
            serde_json::from_value(serde_json::Value::String(id.into())).unwrap();
        let expected = IdOrDefault::Id(id.to_string());

        assert_eq!(actual, expected);
    }

    #[test]
    fn default_parses() {
        let id = "dEfAuLt";

        let actual: IdOrDefault =
            serde_json::from_value(serde_json::Value::String(id.into())).unwrap();
        let expected = IdOrDefault::Default;

        assert_eq!(actual, expected);
    }

    #[test]
    fn manifest_parses() {
        let json = serde_json::json!({
            "source": "manifest",
            "path": "./manifest.json"
        });

        let source: OperationSource = serde_json::from_value(json).unwrap();
        match source {
            OperationSource::Manifest { path } => {
                assert_eq!(path.to_str().unwrap(), "./manifest.json");
            }
            other => panic!("Expected Manifest variant, got {:?}", other),
        }
    }
}
