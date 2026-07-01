use std::collections::HashMap;

use apollo_mcp_server::operations::{AnnotationOverrides, MutationMode};
use apollo_mcp_server::scope_requirements::OperationRequiredScopes;
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

    /// Select which GraphQL mutation operation types the MCP server exposes. This configuration
    /// gate does not provide per-invocation approval.
    pub mutation_mode: MutationMode,

    /// Optional map from exact operation name to top-level tool description. Matching entries
    /// override source-derived descriptions regardless of the operation source. Unmatched entries
    /// are ignored and do not affect operation names, input descriptions, or executable documents.
    pub descriptions: HashMap<String, String>,

    /// Optional map from operation name to MCP tool annotation hints.
    /// When provided, these annotations are merged with the auto-detected
    /// defaults for the matching operations.
    #[serde(default)]
    pub annotations: HashMap<String, AnnotationOverrides>,

    /// Per-operation OAuth scope requirements for step-up authorization.
    /// Keys must exactly match the MCP tool name sent in `tools/call`; unmatched
    /// keys impose no additional restriction. Values are lists of required
    /// scopes (all-of), or lists of alternative required scope groups (OR of
    /// AND). Requirements add to any global scope requirement. When a token
    /// lacks the required scopes for an operation, the server returns HTTP 403
    /// with `WWW-Authenticate: Bearer error="insufficient_scope"`.
    #[serde(default)]
    pub required_scopes: HashMap<String, OperationRequiredScopes>,
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
            &OperationRequiredScopes::All(vec!["user:read".to_string()])
        );
        assert_eq!(
            overrides.required_scopes.get("UpdateUser").unwrap(),
            &OperationRequiredScopes::All(vec!["user:write".to_string()])
        );
        assert_eq!(
            overrides.required_scopes.get("DeleteUser").unwrap(),
            &OperationRequiredScopes::All(vec!["user:write".to_string(), "admin".to_string()])
        );
    }

    #[test]
    fn overrides_with_alternative_required_scopes_parses() {
        let json = serde_json::json!({
            "required_scopes": {
                "GetUser": [["user:read"], ["admin"]],
                "DeleteUser": [["user:write", "tenant:admin"], ["admin"]]
            }
        });

        let overrides: Overrides = serde_json::from_value(json).unwrap();
        assert_eq!(
            overrides.required_scopes.get("GetUser").unwrap(),
            &OperationRequiredScopes::AnyOf(vec![
                vec!["user:read".to_string()],
                vec!["admin".to_string()],
            ])
        );
        assert_eq!(
            overrides.required_scopes.get("DeleteUser").unwrap(),
            &OperationRequiredScopes::AnyOf(vec![
                vec!["user:write".to_string(), "tenant:admin".to_string()],
                vec!["admin".to_string()],
            ])
        );
    }

    #[test]
    fn overrides_with_empty_required_scope_alternative_rejects() {
        let json = serde_json::json!({
            "required_scopes": {
                "GetUser": [[]]
            }
        });

        let error = serde_json::from_value::<Overrides>(json).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required_scopes alternatives must not contain empty scope groups")
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
