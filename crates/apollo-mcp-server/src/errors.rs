use crate::introspection::tools::search::IndexingError;
use apollo_compiler::{Schema, ast::Document, validation::WithErrors};
use apollo_federation::error::FederationError;
use apollo_mcp_registry::platform_api::operation_collections::error::CollectionError;
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use rmcp::serde_json;
use std::fmt;
use tokio::task::JoinError;
use url::ParseError;

/// A wrapper around WithErrors that provides safe UTF-8 formatting
/// This avoids the ariadne UTF-8 multibyte character bug
struct SafeWithErrors<'a, T>(&'a WithErrors<T>);

impl<T> fmt::Display for SafeWithErrors<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Extract error messages without using ariadne's Display implementation
        // which has UTF-8 character boundary issues
        let errors = &self.0.errors;

        if errors.is_empty() {
            return write!(f, "Unknown error");
        }

        writeln!(f, "GraphQL validation errors:")?;
        for (i, diagnostic) in errors.iter().enumerate() {
            // Access the diagnostic error directly without using ariadne formatting
            // The diagnostic.error contains the actual error which implements Display
            write!(f, "  {}. {}", i + 1, diagnostic.error)?;
            writeln!(f)?;
        }

        Ok(())
    }
}

/// An error in operation parsing
#[derive(Debug)]
pub enum OperationError {
    GraphQLDocument(Box<WithErrors<Document>>),
    Internal(String),
    MissingName {
        source_path: Option<String>,
        operation: String,
    },
    NoOperations {
        source_path: Option<String>,
    },
    Json(serde_json::Error),
    TooManyOperations {
        source_path: Option<String>,
        count: usize,
    },
    File(std::io::Error),
    Collection(CollectionError),
}

impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperationError::GraphQLDocument(errors) => {
                write!(
                    f,
                    "Could not parse GraphQL document: {}",
                    SafeWithErrors(errors.as_ref())
                )
            }
            OperationError::Internal(msg) => write!(f, "Internal error: {}", msg),
            OperationError::MissingName {
                source_path,
                operation,
            } => {
                write!(
                    f,
                    "{}Operation is missing its required name: {}",
                    source_path
                        .as_ref()
                        .map(|s| format!("{}: ", s))
                        .unwrap_or_default(),
                    operation
                )
            }
            OperationError::NoOperations { source_path } => {
                write!(
                    f,
                    "{}No operations defined",
                    source_path
                        .as_ref()
                        .map(|s| format!("{}: ", s))
                        .unwrap_or_default()
                )
            }
            OperationError::Json(e) => write!(f, "Invalid JSON: {}", e),
            OperationError::TooManyOperations { source_path, count } => {
                write!(
                    f,
                    "{}Too many operations. Expected 1 but got {}",
                    source_path
                        .as_ref()
                        .map(|s| format!("{}: ", s))
                        .unwrap_or_default(),
                    count
                )
            }
            OperationError::File(e) => write!(f, "{}", e),
            OperationError::Collection(e) => write!(f, "Error loading collection: {}", e),
        }
    }
}

impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OperationError::Json(e) => Some(e),
            OperationError::File(e) => Some(e),
            OperationError::Collection(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for OperationError {
    fn from(e: serde_json::Error) -> Self {
        OperationError::Json(e)
    }
}

impl From<std::io::Error> for OperationError {
    fn from(e: std::io::Error) -> Self {
        OperationError::File(e)
    }
}

/// An error in server initialization
#[derive(Debug)]
pub enum ServerError {
    GraphQLDocument(Box<WithErrors<Document>>),
    GraphQLSchema(Box<WithErrors<Schema>>),
    GraphQLDocumentSchema(Box<WithErrors<Document>>),
    Federation(Box<FederationError>),
    Json(serde_json::Error),
    Operation(OperationError),
    ReadFile(std::io::Error),
    HeaderValue(InvalidHeaderValue),
    HeaderName(InvalidHeaderName),
    Header(String),
    CustomScalarConfig(serde_json::Error),
    CustomScalarJsonSchema(String),
    EnvironmentVariable(String),
    NoOperations,
    NoSchema,
    StartupError(JoinError),
    McpInitializeError(Box<rmcp::service::ServerInitializeError>),
    UrlParseError(ParseError),
    Indexing(IndexingError),
    Cors(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::GraphQLDocument(errors) => {
                write!(
                    f,
                    "Could not parse GraphQL document: {}",
                    SafeWithErrors(errors.as_ref())
                )
            }
            ServerError::GraphQLSchema(errors) => {
                write!(
                    f,
                    "Could not parse GraphQL schema: {}",
                    SafeWithErrors(errors.as_ref())
                )
            }
            ServerError::GraphQLDocumentSchema(errors) => {
                write!(
                    f,
                    "Could not parse GraphQL schema: {}",
                    SafeWithErrors(errors.as_ref())
                )
            }
            ServerError::Federation(e) => write!(f, "Federation error in GraphQL schema: {}", e),
            ServerError::Json(e) => write!(f, "Invalid JSON: {}", e),
            ServerError::Operation(e) => write!(f, "Failed to create operation: {}", e),
            ServerError::ReadFile(e) => write!(f, "Could not open file: {}", e),
            ServerError::HeaderValue(e) => write!(f, "invalid header value: {}", e),
            ServerError::HeaderName(e) => write!(f, "invalid header name: {}", e),
            ServerError::Header(msg) => write!(f, "invalid header: {}", msg),
            ServerError::CustomScalarConfig(e) => write!(f, "invalid custom_scalar_config: {}", e),
            ServerError::CustomScalarJsonSchema(msg) => write!(f, "invalid json schema: {}", msg),
            ServerError::EnvironmentVariable(var) => {
                write!(f, "Missing environment variable: {}", var)
            }
            ServerError::NoOperations => {
                write!(f, "You must define operations or enable introspection")
            }
            ServerError::NoSchema => write!(f, "No valid schema was supplied"),
            ServerError::StartupError(e) => write!(f, "Failed to start server: {}", e),
            ServerError::McpInitializeError(e) => {
                write!(f, "Failed to initialize MCP server: {}", e)
            }
            ServerError::UrlParseError(e) => write!(f, "{}", e),
            ServerError::Indexing(e) => write!(f, "Failed to index schema: {}", e),
            ServerError::Cors(msg) => write!(f, "CORS configuration error: {}", msg),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServerError::Federation(e) => Some(e.as_ref()),
            ServerError::Json(e) => Some(e),
            ServerError::Operation(e) => Some(e),
            ServerError::ReadFile(e) => Some(e),
            ServerError::HeaderValue(e) => Some(e),
            ServerError::HeaderName(e) => Some(e),
            ServerError::StartupError(e) => Some(e),
            ServerError::McpInitializeError(e) => Some(e.as_ref()),
            ServerError::UrlParseError(e) => Some(e),
            ServerError::Indexing(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ServerError {
    fn from(e: serde_json::Error) -> Self {
        ServerError::Json(e)
    }
}

impl From<OperationError> for ServerError {
    fn from(e: OperationError) -> Self {
        ServerError::Operation(e)
    }
}

impl From<std::io::Error> for ServerError {
    fn from(e: std::io::Error) -> Self {
        ServerError::ReadFile(e)
    }
}

impl From<InvalidHeaderValue> for ServerError {
    fn from(e: InvalidHeaderValue) -> Self {
        ServerError::HeaderValue(e)
    }
}

impl From<InvalidHeaderName> for ServerError {
    fn from(e: InvalidHeaderName) -> Self {
        ServerError::HeaderName(e)
    }
}

impl From<JoinError> for ServerError {
    fn from(e: JoinError) -> Self {
        ServerError::StartupError(e)
    }
}

impl From<Box<rmcp::service::ServerInitializeError>> for ServerError {
    fn from(e: Box<rmcp::service::ServerInitializeError>) -> Self {
        ServerError::McpInitializeError(e)
    }
}

impl From<IndexingError> for ServerError {
    fn from(e: IndexingError) -> Self {
        ServerError::Indexing(e)
    }
}

/// An MCP tool error
pub type McpError = rmcp::model::ErrorData;
