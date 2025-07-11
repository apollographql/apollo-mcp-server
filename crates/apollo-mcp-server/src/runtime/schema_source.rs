use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

/// Source for upstream GraphQL schema
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum SchemaSource {
    /// Schema should be loaded (and watched) from a local file path
    Local { path: PathBuf },

    /// Fetch the schema from uplink
    #[default]
    Uplink,
}
