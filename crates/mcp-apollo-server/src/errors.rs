use apollo_compiler::{Schema, ast::Document, validation::WithErrors};
use rmcp::serde_json;

/// An error in operation parsing
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("Could not parse GraphQL document: {0}")]
    GraphQLDocument(WithErrors<Document>),

    #[error("Could not parse GraphQL schema: {0}")]
    GraphQLSchema(WithErrors<Schema>),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Operation is missing its required name: {0}")]
    MissingName(String),

    #[error("No operations defined")]
    NoOperations,

    #[error("Invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Too many operations. Expected 1 but got {0}")]
    TooManyOperations(usize),
}

/// An error in server initialization
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Could not parse GraphQL document: {0}")]
    GraphQLDocument(Box<WithErrors<Document>>),

    #[error("Could not parse GraphQL schema: {0}")]
    GraphQLSchema(Box<WithErrors<Schema>>),

    #[error("Invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Failed to create operation: {0}")]
    Operation(#[from] OperationError),

    #[error("Could not open file: {0}")]
    ReadFile(#[from] std::io::Error),
}
