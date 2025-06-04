use crate::operations::RawOperation;
use apollo_mcp_registry::uplink::schema::event::Event as SchemaEvent;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::io;
use std::path::PathBuf;

/// MCP Server events
pub enum Event {
    /// The schema has been updated
    SchemaUpdated(SchemaEvent),

    /// The operations have been updated
    OperationsUpdated(Vec<RawOperation>),

    /// An error occurred when loading operations
    OperationError(io::Error, PathBuf),

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
            Event::OperationError(e, path) => {
                write!(f, "OperationError({:?}, {:?})", e, path)
            }
            Event::Shutdown => {
                write!(f, "Shutdown")
            }
        }
    }
}
