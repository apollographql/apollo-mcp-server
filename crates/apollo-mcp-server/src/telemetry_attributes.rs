use crate::generated::telemetry::{ALL_ATTRS, TelemetryAttribute};
use opentelemetry::Key;
use std::collections::HashSet;

const GEN_AI_TOOL_NAME: Key = Key::from_static_str("gen_ai.tool.name");
const GEN_AI_TOOL_CALL_ARGUMENTS: Key = Key::from_static_str("gen_ai.tool.call.arguments");
const GEN_AI_TOOL_CALL_RESULT: Key = Key::from_static_str("gen_ai.tool.call.result");

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
            TelemetryAttribute::SessionId => {
                Key::from_static_str(TelemetryAttribute::SessionId.as_str())
            }
            TelemetryAttribute::RawOperation => {
                Key::from_static_str(TelemetryAttribute::RawOperation.as_str())
            }
            TelemetryAttribute::ClientName => {
                Key::from_static_str(TelemetryAttribute::ClientName.as_str())
            }
            TelemetryAttribute::ClientVersion => {
                Key::from_static_str(TelemetryAttribute::ClientVersion.as_str())
            }
            TelemetryAttribute::ToolArguments => {
                Key::from_static_str(TelemetryAttribute::ToolArguments.as_str())
            }
            TelemetryAttribute::ToolResult => {
                Key::from_static_str(TelemetryAttribute::ToolResult.as_str())
            }
            TelemetryAttribute::GraphqlQuery => {
                Key::from_static_str(TelemetryAttribute::GraphqlQuery.as_str())
            }
            TelemetryAttribute::GraphqlResponse => {
                Key::from_static_str(TelemetryAttribute::GraphqlResponse.as_str())
            }
        }
    }

    /// Span attribute keys that carry this attribute's value: its `apollo.*`
    /// key and, for tool calls, the matching OpenTelemetry GenAI key.
    pub fn span_keys(self) -> impl Iterator<Item = Key> {
        let gen_ai = match self {
            TelemetryAttribute::ToolName => Some(GEN_AI_TOOL_NAME),
            TelemetryAttribute::ToolArguments => Some(GEN_AI_TOOL_CALL_ARGUMENTS),
            TelemetryAttribute::ToolResult => Some(GEN_AI_TOOL_CALL_RESULT),
            _ => None,
        };
        std::iter::once(self.to_key()).chain(gen_ai)
    }

    pub fn included_attributes(omitted: HashSet<TelemetryAttribute>) -> Vec<TelemetryAttribute> {
        ALL_ATTRS
            .iter()
            .copied()
            .filter(|a| !omitted.contains(a))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::tool_name(TelemetryAttribute::ToolName, vec!["apollo.mcp.tool_name", "gen_ai.tool.name"])]
    #[case::tool_arguments(TelemetryAttribute::ToolArguments, vec!["apollo.mcp.tool_arguments", "gen_ai.tool.call.arguments"])]
    #[case::tool_result(TelemetryAttribute::ToolResult, vec!["apollo.mcp.tool_result", "gen_ai.tool.call.result"])]
    #[case::graphql_query(TelemetryAttribute::GraphqlQuery, vec!["apollo.mcp.graphql_query"])]
    fn span_keys_include_gen_ai_counterpart(
        #[case] attribute: TelemetryAttribute,
        #[case] expected: Vec<&str>,
    ) {
        let keys: Vec<Key> = attribute.span_keys().collect();
        assert_eq!(keys.iter().map(Key::as_str).collect::<Vec<_>>(), expected);
    }
}
