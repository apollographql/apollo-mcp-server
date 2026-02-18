use crate::introspection::tools::search::IndexingError;
use apollo_compiler::{Schema, ast::Document, validation::WithErrors};
use apollo_federation::error::FederationError;
use apollo_mcp_registry::platform_api::operation_collections::error::CollectionError;
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use rmcp::serde_json;
use tokio::task::JoinError;
use url::ParseError;

/// An error in operation parsing
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("Could not parse GraphQL document: {0}")]
    GraphQLDocument(Box<WithErrors<Document>>),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("{0}Operation is missing its required name: {1}", .source_path.as_ref().map(|s| format!("{s}: ")).unwrap_or_default(), operation)]
    MissingName {
        source_path: Option<String>,
        operation: String,
    },

    #[error("{0}No operations defined", .source_path.as_ref().map(|s| format!("{s}: ")).unwrap_or_default())]
    NoOperations { source_path: Option<String> },

    #[error("Invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}Too many operations. Expected 1 but got {1}", .source_path.as_ref().map(|s| format!("{s}: ")).unwrap_or_default(), count)]
    TooManyOperations {
        source_path: Option<String>,
        count: usize,
    },

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
    Federation(Box<FederationError>),

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
    CustomScalarJsonSchema(String),

    #[error("Missing environment variable: {0}")]
    EnvironmentVariable(String),

    #[error("You must define operations or enable introspection")]
    NoOperations,

    #[error("No valid schema was supplied")]
    NoSchema,

    #[error("Failed to start server")]
    StartupError(#[from] JoinError),

    #[error("Failed to initialize MCP server")]
    McpInitializeError(#[from] Box<rmcp::service::ServerInitializeError>),

    #[error(transparent)]
    UrlParseError(ParseError),

    #[error("Failed to index schema: {0}")]
    Indexing(#[from] IndexingError),

    #[error("CORS configuration error: {0}")]
    Cors(String),

    #[error("Failed to load apps: {0}")]
    Apps(String),

    #[error("TLS configuration error: {0}")]
    Tls(#[from] crate::auth::TlsConfigError),

    #[error("Unsupported transport: {0}")]
    UnsupportedTransport(String),

    #[error("There was a problem parsing Rhai scripts on startup.")]
    RhaiError,
}

/// An MCP tool error
pub type McpError = rmcp::model::ErrorData;
