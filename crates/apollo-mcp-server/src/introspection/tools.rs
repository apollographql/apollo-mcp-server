//! MCP tools to allow an AI agent to introspect a GraphQL schema and execute operations.

mod description;
pub(crate) mod execute;
pub(crate) mod introspect;
pub(crate) mod search;
pub(crate) mod validate;

use rmcp::model::{Tool, ToolAnnotations};

/// Annotations for built-in tools that only read in-process schema state.
///
/// MCP: `readOnlyHint` means the tool does not modify its environment;
/// `destructiveHint` is only meaningful when not read-only;
/// `idempotentHint` means repeated calls with the same arguments have no
/// additional effect; `openWorldHint` means the tool may interact with an
/// open world of external entities.
///
/// introspect/search/validate operate against the locally held schema, so
/// they are read-only, non-destructive, idempotent, and closed-world.
fn annotate_schema_lookup_tool(tool: Tool) -> Tool {
    let mut annotations = ToolAnnotations::new().read_only(true).destructive(false);
    annotations.idempotent_hint = Some(true);
    annotations.open_world_hint = Some(false);
    tool.annotate(annotations)
}
