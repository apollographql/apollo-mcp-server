//! MCP tools to allow an AI agent to introspect a GraphQL schema and execute operations.

mod description;
pub(crate) mod execute;
pub(crate) mod introspect;
pub(crate) mod search;
pub(crate) mod validate;

use rmcp::model::ToolAnnotations;

/// Annotations for built-in tools that only inspect local schema state.
///
/// Their domain of interaction is closed: they read the in-memory schema and never reach the
/// configured GraphQL endpoint, unlike `execute` and the operation tools.
fn schema_lookup_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(true)
        .destructive(false)
        .idempotent(true)
        .open_world(false)
}
