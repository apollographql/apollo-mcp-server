use crate::generated::telemetry::{ALL_ATTRS, TelemetryAttribute};
use opentelemetry::Key;
use std::collections::HashSet;

impl TelemetryAttribute {
    pub const fn to_key(self) -> Key {
        match self {
            TelemetryAttribute::ToolName => {
                Key::from_static_str(TelemetryAttribute::ToolName.as_str())
            }
            TelemetryAttribute::OperationId => {
                Key::from_static_str(TelemetryAttribute::OperationId.as_str())
            }
            TelemetryAttribute::OperationSource => {
                Key::from_static_str(TelemetryAttribute::OperationSource.as_str())
            }
            TelemetryAttribute::Success => {
                Key::from_static_str(TelemetryAttribute::Success.as_str())
            }
            TelemetryAttribute::RequestId => {
                Key::from_static_str(TelemetryAttribute::RequestId.as_str())
            }
        }
    }

    pub fn included_attributes(omitted: HashSet<TelemetryAttribute>) -> Vec<TelemetryAttribute> {
        ALL_ATTRS
            .iter()
            .copied()
            .filter(|a| !omitted.contains(a))
            .collect()
    }
}
