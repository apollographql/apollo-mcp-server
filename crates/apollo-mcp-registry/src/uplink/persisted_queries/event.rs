use std::fmt::Debug;
use std::fmt::Formatter;

use tower::BoxError;

/// Persisted Query events
pub enum Event {
    /// The persisted query manifest was updated
    UpdateManifest(Vec<(String, String)>),
    /// A transient error occurred while fetching the manifest; the previous catalog is retained
    ManifestError(BoxError),
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::UpdateManifest(_) => {
                write!(f, "UpdateManifest(<redacted>)")
            }
            Event::ManifestError(e) => {
                write!(f, "ManifestError({e})")
            }
        }
    }
}
