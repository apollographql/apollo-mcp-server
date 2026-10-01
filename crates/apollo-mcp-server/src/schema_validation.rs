//! Validation policy for schemas that aren't supergraphs.

use schemars::JsonSchema;
use serde::Deserialize;

/// How strictly to validate a schema that isn't a supergraph.
///
/// Parse errors always fail, whatever the policy. The API schema derived from a supergraph
/// isn't affected: federation has already validated it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaValidation {
    /// Reject a schema that breaks GraphQL validation rules. A schema that only breaks rules
    /// introduced by the GraphQL September 2025 specification loads with a warning.
    #[default]
    Strict,

    /// Load a schema that breaks any GraphQL validation rule, with a warning. An invalid
    /// schema can cause failures later, for example when validating or executing operations.
    Lenient,
}
