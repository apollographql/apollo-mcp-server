use std::collections::HashMap;

use apollo_mcp_server::operations::{AnnotationOverrides, MutationMode};
use schemars::JsonSchema;
use serde::Deserialize;

/// Overridable flags
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Overrides {
    /// Disable type descriptions to save on context-window space
    pub disable_type_description: bool,

    /// Disable schema descriptions to save on context-window space
    pub disable_schema_description: bool,

    /// Enable output schema generation for tools (adds token overhead but helps LLMs understand response structure)
    pub enable_output_schema: bool,

    /// Expose a tool that returns the URL to open a GraphQL operation in Apollo Explorer (requires APOLLO_GRAPH_REF)
    pub enable_explorer: bool,

    /// Set the mutation mode access level for the MCP server
    pub mutation_mode: MutationMode,

    /// Optional map from operation name to tool description. When provided,
    /// these descriptions override the auto-generated tool descriptions for
    /// the matching operations, regardless of the operation source.
    pub descriptions: HashMap<String, String>,

    /// Optional map from operation name to MCP tool annotation hints.
    /// When provided, these annotations are merged with the auto-detected
    /// defaults for the matching operations.
    #[serde(default)]
    pub annotations: HashMap<String, AnnotationOverrides>,

    /// Per-operation OAuth scope requirements for step-up authorization.
    /// Keys are operation names; values are lists of required scopes.
    /// When a token lacks the required scopes for an operation, the server
    /// returns HTTP 403 with `WWW-Authenticate: Bearer error="insufficient_scope"`.
    #[serde(default)]
    pub required_scopes: HashMap<String, Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_with_descriptions_parses() {
        let json = serde_json::json!({
            "descriptions": {
                "GetAlerts": "Fetch active weather alerts",
                "GetForecast": "Get the 7-day forecast"
            }
        });

        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert_eq!(
            overrides.descriptions,
            HashMap::from([
                (
                    "GetAlerts".to_string(),
                    "Fetch active weather alerts".to_string()
                ),
                (
                    "GetForecast".to_string(),
                    "Get the 7-day forecast".to_string()
                ),
            ])
        );
    }

    #[test]
    fn overrides_without_descriptions_defaults_to_empty() {
        let json = serde_json::json!({});
        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert!(overrides.descriptions.is_empty());
    }

    #[test]
    fn overrides_with_required_scopes_parses() {
        let json = serde_json::json!({
            "required_scopes": {
                "GetUser": ["user:read"],
                "UpdateUser": ["user:write"],
                "DeleteUser": ["user:write", "admin"]
            }
        });

        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert_eq!(
            overrides.required_scopes.get("GetUser").unwrap(),
            &vec!["user:read".to_string()]
        );
        assert_eq!(
            overrides.required_scopes.get("UpdateUser").unwrap(),
            &vec!["user:write".to_string()]
        );
        assert_eq!(
            overrides.required_scopes.get("DeleteUser").unwrap(),
            &vec!["user:write".to_string(), "admin".to_string()]
        );
    }

    #[test]
    fn overrides_without_required_scopes_defaults_to_empty() {
        let json = serde_json::json!({});
        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert!(overrides.required_scopes.is_empty());
    }

    #[test]
    fn overrides_with_annotations_parses() {
        let json = serde_json::json!({
            "annotations": {
                "GetAlerts": {
                    "read_only_hint": true,
                    "idempotent_hint": true
                },
                "CreateUser": {
                    "destructive_hint": false,
                    "title": "Create a new user account"
                }
            }
        });

        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert_eq!(overrides.annotations.len(), 2);

        let alerts = overrides.annotations.get("GetAlerts").unwrap();
        assert_eq!(alerts.read_only_hint, Some(true));
        assert_eq!(alerts.idempotent_hint, Some(true));
        assert_eq!(alerts.destructive_hint, None);
        assert_eq!(alerts.title, None);
        assert_eq!(alerts.open_world_hint, None);

        let create_user = overrides.annotations.get("CreateUser").unwrap();
        assert_eq!(create_user.destructive_hint, Some(false));
        assert_eq!(
            create_user.title.as_deref(),
            Some("Create a new user account")
        );
        assert_eq!(create_user.read_only_hint, None);
    }

    #[test]
    fn overrides_without_annotations_defaults_to_empty() {
        let json = serde_json::json!({});
        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert!(overrides.annotations.is_empty());
    }

    #[test]
    fn annotation_overrides_rejects_unknown_fields() {
        let json = serde_json::json!({
            "annotations": {
                "GetAlerts": {
                    "unknown_hint": true
                }
            }
        });

        let result = serde_json::from_value::<Overrides>(json);
        assert!(result.is_err());
    }
}
