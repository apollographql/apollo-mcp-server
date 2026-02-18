use super::SchemaState;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::fmt::Result;

/// Schema events
pub enum Event {
    /// The schema was updated.
    UpdateSchema(SchemaState),

    /// There are no more updates to the schema
    NoMoreSchema,
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter) -> Result {
        match self {
            Event::UpdateSchema(_) => {
                write!(f, "UpdateSchema(<redacted>)")
            }
            Event::NoMoreSchema => {
                write!(f, "NoMoreSchema")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_event_no_more_schema() {
        let event = Event::NoMoreSchema;
        let output = format!("{:?}", event);
        assert_eq!(output, "NoMoreSchema");
    }

    #[test]
    fn debug_redacts_update_schema() {
        let event = Event::UpdateSchema(SchemaState {
            sdl: "type Query { hello: String }".to_string(),
            launch_id: Some("test-launch-123".to_string()),
        });

        let output = format!("{:?}", event);
        assert_eq!(output, "UpdateSchema(<redacted>)");
        assert!(!output.contains("type Query"));
        assert!(!output.contains("test-launch-123"));
    }
}
