use crate::errors::McpError;
use crate::schema_from_type;
use base64::Engine;
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;

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
    variables: String,
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
        let compressed = lz_str::compress_to_uint8_array(input.to_string().as_str());
        let encoded = base64::engine::general_purpose::STANDARD.encode(compressed);
        format!(
            "https://studio.apollographql.com/graph/{graph_id}/variant/{variant}/explorer?explorerURLState={encoded}",
            graph_id = self.graph_id,
            variant = self.variant
        )
    }

    pub async fn execute(&self, input: Value) -> Result<CallToolResult, McpError> {
        webbrowser::open(self.create_explorer_url(&input).as_str())
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
    use rmcp::serde_json::json;

    #[test]
    fn test_create_explorer_url() {
        let explorer = Explorer::new(String::from("mcp-example@mcp"));
        let input = json!({
            "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
            "headers": "{}",
            "variables": "{\"state\": \"CA\"}"
        });

        let url = explorer.create_explorer_url(&input);
        assert_eq!(
            url,
            "https://studio.apollographql.com/graph/mcp-example/variant/mcp/explorer?explorerURLState=N4IgJg9gxgrgtgUwHYBcQC4QEcYIE4CeABAOIIoDqCAhigBb4CCANvigM4AUAJOyrQnREAyijwBLJAHMAhAEoiwADpIiRaqzwdOfAUN78UCBctVqi7BADd84lARXmiYBOygSADinEQkj85J8eDBQ3r7+AL4qESAANCAM1C547BggwDHxVtQS1ABGrKmYyiC6RkoYRBUAwowVMRFAAAA="
        );
    }
}
