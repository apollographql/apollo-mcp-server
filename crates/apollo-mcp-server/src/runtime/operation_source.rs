use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

/// Source for loaded operations
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationSource {
    /// Load operations from a GraphOS collection
    Collection {
        #[schemars(with = "String")]
        id: IdOrDefault,
    },

    /// Load operations from local GraphQL files / folders
    Local { paths: Vec<PathBuf> },

    /// Load operations from a persisted queries manifest file
    Manifest { path: PathBuf },

    /// Load operations from uplink
    Uplink,

    /// No configuration specified
    #[default]
    Unspecified,
}

/// Either a custom ID or the default variant
#[derive(Debug)]
pub enum IdOrDefault {
    /// The ddefault tools for the variant (requires APOLLO_KEY)
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
