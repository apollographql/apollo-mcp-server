use apollo_compiler::{Schema, ast::Document, validation::WithErrors};
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use rmcp::serde_json;

/// An error in operation parsing
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("Could not parse GraphQL document: {0}")]
    GraphQLDocument(Box<WithErrors<Document>>),

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

    #[error(transparent)]
    File(#[from] std::io::Error),
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

    #[error("invalid header value: {0}")]
    HeaderValue(#[from] InvalidHeaderValue),

    #[error("invalid header name: {0}")]
    HeaderName(#[from] InvalidHeaderName),

    #[error("invalid header: {0}")]
    Header(String),
}
