use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Deserialize, Serialize, PartialEq, Copy, JsonSchema)]
#[serde(rename_all = "snake_case")]
/// Controls which GraphQL mutation operation types Apollo MCP Server exposes.
///
/// This is a configuration gate, not a per-invocation approval mechanism.
pub enum MutationMode {
    /// Block mutation operations. Predefined mutations from configured operation sources are
    /// skipped, and ad hoc mutations submitted to the `execute` tool are rejected.
    #[default]
    None,
    /// Allow predefined mutations loaded from configured operation sources, but reject ad hoc
    /// mutations submitted to the `execute` tool.
    Explicit,
    /// Allow predefined mutations and ad hoc mutations submitted to an enabled `execute` tool.
    All,
}
