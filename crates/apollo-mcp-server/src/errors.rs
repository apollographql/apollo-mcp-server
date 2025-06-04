use apollo_compiler::{Schema, ast::Document, validation::WithErrors};
use apollo_federation::error::FederationError;
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use rmcp::serde_json;
use tokio::task::JoinError;

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error(transparent)]
    HeaderName(InvalidHeaderName),

    #[error(transparent)]
    HeaderValue(InvalidHeaderValue),

    #[error(transparent)]
    Request(reqwest::Error),

    #[error("Error in response: {0}")]
    Response(String),

    #[error("Invalid variables: {0}")]
    InvalidVariables(String),
}

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

    #[error("Error loading collection: {0}")]
    Collection(CollectionError),
}

/// An error in server initialization
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Could not parse GraphQL document: {0}")]
    GraphQLDocument(Box<WithErrors<Document>>),

    #[error("Could not parse GraphQL schema: {0}")]
    GraphQLSchema(Box<WithErrors<Schema>>),

    #[error("Could not parse GraphQL schema: {0}")]
    GraphQLDocumentSchema(Box<WithErrors<Document>>),

    #[error("Federation error in GraphQL schema: {0}")]
    Federation(FederationError),

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

    #[error("invalid custom_scalar_config: {0}")]
    CustomScalarConfig(serde_json::Error),

    #[error("invalid json schema: {0}")]
    CustomScalarJsonSchema(serde_json::Value),

    #[error("Missing environment variable: {0}")]
    EnvironmentVariable(String),

    #[error("You must define operations or enable introspection")]
    NoOperations,

    #[error("No valid schema was supplied")]
    NoSchema,

    #[error("Failed to start server")]
    StartupError(#[from] JoinError),

    #[error("Failed to initialize MCP server")]
    McpInitializeError(#[from] rmcp::service::ServerInitializeError<std::io::Error>),
}

/// An MCP tool error
pub type McpError = rmcp::model::ErrorData;
