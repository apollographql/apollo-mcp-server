use crate::errors::McpError;
use crate::schema_from_type;
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::{Deserialize, Serialize};
use tracing::debug;
use tracing::log::Level::Debug;
use tracing::log::log_enabled;

pub(crate) const EXPLORER_TOOL_NAME: &str = "explorer";

#[derive(Clone)]
pub struct Explorer {
    graph_id: String,
    variant: String,
    pub tool: Tool,
}

#[derive(JsonSchema, Deserialize, Serialize)]
pub struct Input {
    /// The GraphQL document
    #[serde(default = "default_input")]
    document: String,

    /// Any variables used in the document
    #[serde(default = "default_input")]
    variables: String,

    /// Headers to be sent with the operation
    #[serde(default = "default_input")]
    headers: String,
}

fn default_input() -> String {
    "{}".to_string()
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
                "Get the URL to open a GraphQL operation in Apollo Explorer",
                schema_from_type!(Input),
            ),
        }
    }

    fn create_explorer_url(&self, input: Input) -> Result<String, McpError> {
        serde_json::to_string(&input)
            .map(|serialized| lz_str::compress_to_encoded_uri_component(serialized.as_str()))
            .map(|compressed| {
                format!(
                    "https://studio.apollographql.com/graph/{graph_id}/variant/{variant}/explorer?explorerURLState={compressed}",
                    graph_id = self.graph_id,
                    variant = self.variant,
                )
            })
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Unable to serialize input: {e}"),
                    None,
                )
            })
    }

    pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
        let pretty = if log_enabled!(Debug) {
            Some(serde_json::to_string_pretty(&input).unwrap_or("<unable to serialize>".into()))
        } else {
            None
        };
        let url = self.create_explorer_url(input)?;
        debug!(?url, input=?pretty, "Created URL to open operation in Apollo Explorer");
        Ok(CallToolResult {
            content: vec![Content::text(url)],
            is_error: None,
        })
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
            "variables": "{\"state\": \"CO\"}",
            "headers": "{\"x-foo\": \"bar\"}",
        });

        let input: Input = serde_json::from_value(input).unwrap();

        let url = explorer.create_explorer_url(input).unwrap();
        assert_snapshot!(
            url,
            @"https://studio.apollographql.com/graph/mcp-example/variant/mcp/explorer?explorerURLState=N4IgJg9gxgrgtgUwHYBcQC4QEcYIE4CeABAOIIoDqCAhigBb4CCANvigM4AUAJOyrQnREAyijwBLJAHMAhAEoiwADpIiRaqzwdOfAUN78UCBctVqi7BADd84lARXmiYBOygSADinEQkj85J8eDBQ3r7+AL4qESAANCBW1BLUAEas7BggyiC6RkoYRPkAwgDy+THxDNQueBmY2QAeALQAZhAQ+UL5KUnlIBFAA"
        );
    }

    #[tokio::test]
    #[rstest]
    #[case(json!({
        "variables": "{\"state\": \"CA\"}",
        "headers": "{}"
    }), json!({
        "document": "{}",
        "variables": "{\"state\": \"CA\"}",
        "headers": "{}"
    }))]
    #[case(json!({
        "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
        "headers": "{}"
    }), json!({
        "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
        "variables": "{}",
        "headers": "{}"
    }))]
    #[case(json!({
        "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
        "variables": "{\"state\": \"CA\"}",
    }), json!({
        "document": "query GetWeatherAlerts($state: String!) {\n  alerts(state: $state) {\n    severity\n    description\n    instruction\n  }\n}",
        "variables": "{\"state\": \"CA\"}",
        "headers": "{}"
    }))]
    async fn test_input_missing_fields(#[case] input: Value, #[case] input_with_default: Value) {
        let input = serde_json::from_value::<Input>(input).unwrap();
        let input_with_default = serde_json::from_value::<Input>(input_with_default).unwrap();
        let explorer = Explorer::new(String::from("mcp-example@mcp"));
        assert_eq!(
            explorer.create_explorer_url(input),
            explorer.create_explorer_url(input_with_default)
        );
    }
}
