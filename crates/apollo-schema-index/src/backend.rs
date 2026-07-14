//! The search backend contract shared by lexical and (Phase 2) semantic search.

use crate::OperationType;
use crate::error::SearchError;
use crate::path::Scored;

/// A retrievable operation: a root Query/Mutation field the agent can invoke.
/// Identity for fusion/dedupe is `(operation_type, field_name)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OperationRef {
    pub operation_type: OperationType,
    pub field_name: String,
    pub return_type: Option<String>,
    pub arg_types: Vec<String>,
}

impl std::fmt::Display for OperationRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let root = match self.operation_type {
            OperationType::Query => "Query",
            OperationType::Mutation => "Mutation",
            OperationType::Subscription => "Subscription",
        };
        write!(f, "{root}.{}", self.field_name)?;
        if !self.arg_types.is_empty() {
            write!(f, "({})", self.arg_types.join(", "))?;
        }
        if let Some(rt) = &self.return_type {
            write!(f, ": {rt}")?;
        }
        Ok(())
    }
}

/// A search backend over a GraphQL schema's operations.
pub trait SchemaSearch {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ref_display() {
        let op = OperationRef {
            operation_type: OperationType::Query,
            field_name: "userByEmail".to_string(),
            return_type: Some("TargetUser".to_string()),
            arg_types: vec!["String".to_string()],
        };
        assert_eq!(op.to_string(), "Query.userByEmail(String): TargetUser");
    }

    #[test]
    fn operation_ref_identity_ignores_ordering_of_equal_refs() {
        let a = OperationRef {
            operation_type: OperationType::Query,
            field_name: "x".into(),
            return_type: None,
            arg_types: vec![],
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
