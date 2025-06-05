use std::fmt::Debug;
use std::fmt::Formatter;

/// Persisted Query events
pub enum Event {
    /// The persisted query manifest was updated
    UpdateManifest(Vec<(String, String, String)>),
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::UpdateManifest(_) => {
                write!(f, "UpdateManifest(<redacted>)")
            }
        }
    }
}
