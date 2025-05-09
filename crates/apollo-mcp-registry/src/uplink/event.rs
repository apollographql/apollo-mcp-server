use crate::uplink::schema::SchemaState;
use std::fmt::Debug;
use std::fmt::Formatter;

/// Messages that are broadcast across the app.
pub enum Event {
    /// The schema was updated.
    UpdateSchema(SchemaState),

    /// There are no more updates to the schema
    NoMoreSchema,

    /// The server should gracefully shut down.
    Shutdown,
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::UpdateSchema(_) => {
                write!(f, "UpdateSchema(<redacted>)")
            }
            Event::NoMoreSchema => {
                write!(f, "NoMoreSchema")
            }
            Event::Shutdown => {
                write!(f, "Shutdown")
            }
        }
    }
}
