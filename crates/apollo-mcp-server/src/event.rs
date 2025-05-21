use crate::operations::RawOperation;
use apollo_mcp_registry::uplink::schema::event::Event as SchemaEvent;
use std::fmt::Debug;
use std::fmt::Formatter;

/// MCP Server events
pub enum Event {
    /// The schema has been updated
    SchemaUpdated(SchemaEvent),

    /// The operations have been updated
    OperationsUpdated(Vec<RawOperation>),

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
            Event::Shutdown => {
                write!(f, "Shutdown")
            }
        }
    }
}
