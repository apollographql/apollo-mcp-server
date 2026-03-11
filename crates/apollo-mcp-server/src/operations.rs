//! Operations
//!
//! This module includes transformation utilities that convert GraphQL operations
//! into MCP tools.

mod execution;
mod mutation_mode;
mod operation;
mod operation_source;
mod raw_operation;
mod schema_walker;

use std::collections::HashMap;

pub(crate) use execution::{execute_operation, find_and_execute_operation};
pub use mutation_mode::MutationMode;
pub(crate) use operation::{Operation, operation_defs, operation_name};
pub use operation_source::OperationSource;
pub(crate) use operation_source::extract_operation_name;
pub(crate) use raw_operation::RawOperation;

/// If an override description exists for this operation, set it on the raw
/// operation so it takes priority over auto-generated descriptions.
pub(crate) fn apply_description_override(
    mut operation: RawOperation,
    descriptions: &HashMap<String, String>,
) -> RawOperation {
    if let Some(desc) =
        extract_operation_name(&operation.source_text).and_then(|name| descriptions.get(name))
    {
        operation.description = Some(desc.clone());
    }
    operation
}
