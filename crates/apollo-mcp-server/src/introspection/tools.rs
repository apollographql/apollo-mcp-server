//! MCP tools to allow an AI agent to introspect a GraphQL schema and execute operations.

mod description;
pub(crate) mod execute;
pub(crate) mod introspect;
pub(crate) mod search;
pub(crate) mod validate;

use rmcp::model::{Tool, ToolAnnotations};

/// Annotate built-in tools that only inspect local schema state.
fn annotate_schema_lookup_tool(tool: Tool) -> Tool {
    let mut annotations = ToolAnnotations::new().read_only(true).destructive(false);
    annotations.idempotent_hint = Some(true);
    annotations.open_world_hint = Some(false);
    tool.annotate(annotations)
}
