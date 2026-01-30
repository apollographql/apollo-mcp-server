use crate::operations::operation_defs;
use crate::schema_from_type;
use apollo_compiler::Schema;
use apollo_compiler::parser::Parser;
use apollo_compiler::validation::Valid;
use rmcp::model::CallToolResult;
use rmcp::model::Content;
use rmcp::model::Tool;
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;

/// The name of the tool to validate an ad hoc GraphQL operation
pub const VALIDATE_TOOL_NAME: &str = "validate";

#[derive(Clone)]
pub struct Validate {
    pub tool: Tool,
    schema: Arc<RwLock<Valid<Schema>>>,
}

/// Input for the validate tool
#[derive(JsonSchema, Deserialize, Debug)]
pub struct Input {
    /// The GraphQL operation
    operation: String,
}

impl Validate {
    pub fn new(schema: Arc<RwLock<Valid<Schema>>>) -> Self {
        Self {
            schema,
            tool: Tool::new(
                VALIDATE_TOOL_NAME,
                "Validates a GraphQL operation against the schema. \
                Use the `introspect` tool first to get information about the GraphQL schema. \
                Operations should be validated prior to calling the `execute` tool.",
                schema_from_type!(Input),
            ),
        }
    }

    /// Validates the provided GraphQL query
    #[tracing::instrument(skip(self), ret)]
    pub async fn execute(&self, input: Value) -> CallToolResult {
        let input = match serde_json::from_value::<Input>(input) {
            Ok(i) => i,
            Err(e) => {
                return CallToolResult::error(vec![Content::text(format!("Invalid input: {e}"))]);
            }
        };

        if let Err(e) = operation_defs(&input.operation, true, None) {
            return CallToolResult::error(vec![Content::text(e.to_string())]);
        }

        if operation_defs(&input.operation, true, None)
            .ok()
            .flatten()
            .is_none()
        {
            return CallToolResult::error(vec![Content::text("Invalid operation type")]);
        }

        let schema_guard = self.schema.read().await;
        match Parser::new()
            .parse_executable(&schema_guard, input.operation.as_str(), "operation.graphql")
            .and_then(|p| p.validate(&schema_guard))
        {
            Ok(_) => CallToolResult::success(vec![Content::text("Operation is valid")]),
            Err(e) => CallToolResult::error(vec![Content::text(e.to_string())]),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    static SCHEMA: std::sync::LazyLock<Arc<RwLock<Valid<Schema>>>> =
        std::sync::LazyLock::new(|| {
            Arc::new(RwLock::new(
                Schema::parse_and_validate(
                    "type Query { id: ID! hello(name: String!): String! }",
                    "schema.graphql",
                )
                .unwrap(),
            ))
        });

    #[tokio::test]
    async fn validate_valid_query() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query Test { id }"
        });
        let result = validate.execute(input).await;
        assert!(result.is_error.is_none() || !result.is_error.unwrap());
    }

    #[tokio::test]
    async fn validate_invalid_graphql_query() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query {"
        });
        let result = validate.execute(input).await;
        assert!(result.is_error == Some(true));
    }

    #[tokio::test]
    async fn validate_invalid_query_field() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query { invalidField }"
        });
        let result = validate.execute(input).await;
        assert!(result.is_error == Some(true));
    }

    #[tokio::test]
    async fn validate_invalid_argument() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query { hello }"
        });
        let result = validate.execute(input).await;
        assert!(result.is_error == Some(true));
    }
}
