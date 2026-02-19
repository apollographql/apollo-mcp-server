use crate::operations::RawOperation;
use apollo_mcp_registry::platform_api::operation_collections::error::CollectionError;
use apollo_mcp_registry::uplink::schema::event::Event as SchemaEvent;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::fmt::Result;
use std::io;

/// MCP Server events
pub enum Event {
    /// The schema has been updated
    SchemaUpdated(SchemaEvent),

    /// The operations have been updated
    OperationsUpdated(Vec<RawOperation>),

    /// An error occurred when loading operations
    OperationError(io::Error, Option<String>),

    /// An error occurred when loading operations from collection
    CollectionError(CollectionError),

    /// The server should gracefully shut down
    Shutdown,
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter) -> Result {
        match self {
            Event::SchemaUpdated(event) => {
                write!(f, "SchemaUpdated({event:?})")
            }
            Event::OperationsUpdated(operations) => {
                write!(f, "OperationsChanged({operations:?})")
            }
            Event::OperationError(e, path) => {
                write!(f, "OperationError({e:?}, {path:?})")
            }
            Event::CollectionError(e) => {
                write!(f, "OperationError({e:?})")
            }
            Event::Shutdown => {
                write!(f, "Shutdown")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_event_schema_updated() {
        let event = Event::SchemaUpdated(SchemaEvent::NoMoreSchema);
        let output = format!("{:?}", event);
        assert_eq!(output, "SchemaUpdated(NoMoreSchema)");
    }

    #[test]
    fn debug_event_operations_updated() {
        let event = Event::OperationsUpdated(vec![]);
        let output = format!("{:?}", event);
        assert_eq!(output, "OperationsChanged([])");
    }

    #[test]
    fn debug_event_operation_error() {
        let event = Event::OperationError(std::io::Error::other("TEST"), None);
        let output = format!("{:?}", event);
        assert_eq!(
            output,
            r#"OperationError(Custom { kind: Other, error: "TEST" }, None)"#
        );
    }

    #[test]
    fn debug_event_collection_error() {
        let event = Event::CollectionError(CollectionError::Response("TEST".to_string()));
        let output = format!("{:?}", event);
        assert_eq!(output, r#"OperationError(Response("TEST"))"#);
    }

    #[test]
    fn debug_event_shutdown() {
        let event = Event::Shutdown;
        let output = format!("{:?}", event);
        assert_eq!(output, "Shutdown");
    }
}
