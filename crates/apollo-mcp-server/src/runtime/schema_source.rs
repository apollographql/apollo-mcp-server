use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

/// Source for upstream GraphQL schema
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum SchemaSource {
    /// Schema should be loaded (and watched) from a local file path
    Local { path: PathBuf },

    /// Fetch the schema from uplink
    #[default]
    Uplink,

    /// Fetch the latest published schema from the GraphOS Platform API.
    /// Unlike uplink, this works for non-federated graphs.
    Graphos,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_graphos_source() {
        let source: SchemaSource = serde_yaml::from_str("source: graphos").unwrap();
        assert!(matches!(source, SchemaSource::Graphos));
    }
}
