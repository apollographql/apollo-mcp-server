use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Deserialize, Serialize, PartialEq, Copy, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MutationMode {
    /// Don't allow any mutations
    #[default]
    None,
    /// Allow explicit mutations, but don't allow the LLM to build them
    Explicit,
    /// Allow the LLM to build mutations
    All,
}
