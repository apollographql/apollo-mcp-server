use crate::operations::RawOperation;
use apollo_mcp_registry::uplink::schema::event::Event as SchemaEvent;
use reqwest::header::InvalidHeaderName;
use reqwest::header::InvalidHeaderValue;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::io;

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

/// MCP Server events
pub enum Event {
    /// The schema has been updated
    SchemaUpdated(SchemaEvent),

    /// The operations have been updated
    OperationsUpdated(Vec<RawOperation>),

    /// An error occurred when loading operations
    OperationError(io::Error),

    /// An error occurred when loading operations
    CollectionError(CollectionError),

    /// The server should gracefully shut down
    Shutdown,
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::SchemaUpdated(event) => {
                write!(f, "SchemaUpdated({:?})", event)
            }
            Event::OperationsUpdated(operations) => {
                write!(f, "OperationsChanged({:?})", operations)
            }
            Event::OperationError(e) => {
                write!(f, "OperationError({:?})", e)
            }
            Event::CollectionError(e) => {
                write!(f, "OperationError({:?})", e)
            }
            Event::Shutdown => {
                write!(f, "Shutdown")
            }
        }
    }
}
