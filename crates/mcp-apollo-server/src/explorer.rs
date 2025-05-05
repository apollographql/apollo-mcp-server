use crate::errors::McpError;
use crate::schema_from_type;
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use tracing::info;

pub(crate) const EXPLORER_TOOL_NAME: &str = "explorer";

#[derive(Clone)]
pub struct Explorer {
    graph_id: String,
    variant: String,
    pub tool: Tool,
}

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)] // This is only used to generate the JSON schema
pub struct Input {
    /// The GraphQL document
    document: String,

    /// Any variables used in the document
    variables: String,

    /// Headers to be sent with the operation
    headers: String,
}

impl Explorer {
    pub fn new(graph_ref: String) -> Self {
        let (graph_id, variant) = match graph_ref.split_once('@') {
            Some((graph_id, variant)) => (graph_id.to_string(), variant.to_string()),
            None => (graph_ref, String::from("current")),
        };
        Self {
            graph_id,
            variant,
            tool: Tool::new(
                EXPLORER_TOOL_NAME,
                "Open a GraphQL operation in Apollo Explorer",
                schema_from_type!(Input),
            ),
        }
    }

    fn create_explorer_url(&self, input: &Value) -> String {
        let mut input = input.clone();

        let document = input.get("document").and_then(|v| v.as_str());
        if document.is_none() || document == Some("") {
            if let Some(obj) = input.as_object_mut() {
                obj.insert("document".to_string(), Value::String("{}".to_string()));
            }
        }
        let variables = input.get("variables").and_then(|v| v.as_str());
        if variables.is_none() || variables == Some("") {
            if let Some(obj) = input.as_object_mut() {
                obj.insert("variables".to_string(), Value::String("{}".to_string()));
            }
        }
        let headers = input.get("headers").and_then(|v| v.as_str());
        if headers.is_none() || headers == Some("") {
            if let Some(obj) = input.as_object_mut() {
                obj.insert("headers".to_string(), Value::String("{}".to_string()));
            }
        }
        let compressed = lz_str::compress_to_encoded_uri_component(input.to_string().as_str());
        format!(
            "https://studio.apollographql.com/graph/{graph_id}/variant/{variant}/explorer?explorerURLState={compressed}",
            graph_id = self.graph_id,
            variant = self.variant
        )
    }

    pub async fn execute(&self, input: Value) -> Result<CallToolResult, McpError> {
        let url = self.create_explorer_url(&input);
        info!(
            "Opening Apollo Explorer URL: {} for input operation: {}",
            url,
            serde_json::to_string_pretty(&input).unwrap_or("<unable to serialize>".into())
        );
        webbrowser::open(url.as_str())
            .map(|_| CallToolResult {
                content: vec![Content::text("success")],
                is_error: None,
            })
            .map_err(|_| McpError::new(ErrorCode::INTERNAL_ERROR, "Unable to open browser", None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use rmcp::serde_json::json;
    use rstest::rstest;

    #[test]
    fn test_create_explorer_url() {
        let explorer = Explorer::new(String::from("mcp-example@mcp"));
        let input = json!({
            "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
            "variables": "{\"state\": \"CA\"}",
            "headers": "{}"
        });

        let url = explorer.create_explorer_url(&input);
        assert_snapshot!(
            url,
            @"https://studio.apollographql.com/graph/mcp-example/variant/mcp/explorer?explorerURLState=N4IgJg9gxgrgtgUwHYBcQC4QEcYIE4CeABAOIIoDqCAhigBb4CCANvigM4AUAJOyrQnREAyijwBLJAHMAhAEoiwADpIiRaqzwdOfAUN78UCBctVqi7BADd84lARXmiYBOygSADinEQkj85J8eDBQ3r7+AL4qESAANCAM1C547BggwDHxVtQS1ABGrKmYyiC6RkoYRBUAwowVMRFAA"
        );
    }

    #[tokio::test]
    #[rstest]
    #[case(json!({
        "variables": "{\"state\": \"CA\"}",
        "headers": "{}"
    }), "document")]
    #[case(json!({
        "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
        "headers": "{}"
    }), "variables")]
    #[case(json!({
        "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
        "variables": "{\"state\": \"CA\"}"
    }), "headers")]
    async fn test_input_missing_fields(#[case] input: Value, #[case] missing_field: &str) {
        let explorer = Explorer::new(String::from("mcp-example@mcp"));
        let url = explorer.create_explorer_url(&input);
        let filled_input = {
            let mut input = input;
            if missing_field == "document" {
                if let Some(obj) = input.as_object_mut() {
                    obj.insert("document".to_string(), Value::String("{}".to_string()));
                }
            }
            if missing_field == "variables" {
                if let Some(obj) = input.as_object_mut() {
                    obj.insert("variables".to_string(), Value::String("{}".to_string()));
                }
            }
            if missing_field == "headers" {
                if let Some(obj) = input.as_object_mut() {
                    obj.insert("headers".to_string(), Value::String("{}".to_string()));
                }
            }
            input
        };
        let expected_url = explorer.create_explorer_url(&filled_input);
        assert_eq!(url, expected_url);
    }
}
