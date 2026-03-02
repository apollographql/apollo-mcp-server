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

/// If per-operation OAuth scope requirements exist for this operation, set them
/// on the raw operation for step-up authorization (MCP spec 2025-11-25).
pub(crate) fn apply_required_scopes_override(
    mut operation: RawOperation,
    required_scopes: &HashMap<String, Vec<String>>,
) -> RawOperation {
    if let Some(scopes) =
        extract_operation_name(&operation.source_text).and_then(|name| required_scopes.get(name))
    {
        operation.required_scopes = scopes.clone();
    }
    operation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_required_scopes_override_sets_scopes_for_matching_operation() {
        let operation =
            RawOperation::from(("query GetUser { user { id } }".to_string(), None::<String>));
        let required_scopes =
            HashMap::from([("GetUser".to_string(), vec!["user:read".to_string()])]);

        let result = apply_required_scopes_override(operation, &required_scopes);
        assert_eq!(result.required_scopes, vec!["user:read".to_string()]);
    }

    #[test]
    fn apply_required_scopes_override_leaves_unmatched_operations_unchanged() {
        let operation = RawOperation::from((
            "query ListUsers { users { id } }".to_string(),
            None::<String>,
        ));
        let required_scopes =
            HashMap::from([("GetUser".to_string(), vec!["user:read".to_string()])]);

        let result = apply_required_scopes_override(operation, &required_scopes);
        assert!(result.required_scopes.is_empty());
    }

    #[test]
    fn apply_required_scopes_override_with_empty_map_leaves_scopes_empty() {
        let operation =
            RawOperation::from(("query GetUser { user { id } }".to_string(), None::<String>));

        let result = apply_required_scopes_override(operation, &HashMap::new());
        assert!(result.required_scopes.is_empty());
    }
}
