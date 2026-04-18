use rmcp::model::ToolAnnotations;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Per-operation MCP tool annotation hints.
///
/// Each field is optional. Only the hints you specify override the
/// auto-detected defaults; unset fields keep their original values.
#[derive(Debug, Deserialize, Serialize, Default, JsonSchema, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct AnnotationOverrides {
    pub title: Option<String>,
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub idempotent_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
}

impl AnnotationOverrides {
    /// Merge user-specified overrides into `base`, replacing only fields that
    /// are `Some` in `self`.
    pub fn apply_to(&self, base: &mut ToolAnnotations) {
        if let Some(ref title) = self.title {
            base.title = Some(title.clone());
        }
        if let Some(v) = self.read_only_hint {
            base.read_only_hint = Some(v);
        }
        if let Some(v) = self.destructive_hint {
            base.destructive_hint = Some(v);
        }
        if let Some(v) = self.idempotent_hint {
            base.idempotent_hint = Some(v);
        }
        if let Some(v) = self.open_world_hint {
            base.open_world_hint = Some(v);
        }
    }
}
