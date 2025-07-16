use crate::errors::McpError;
use crate::operations::operation_defs;
use crate::schema_from_type;
use apollo_compiler::Schema;
use apollo_compiler::parser::Parser;
use apollo_compiler::validation::Valid;
use rmcp::model::CallToolResult;
use rmcp::model::Content;
// Import trait for Valid::new
use rmcp::model::{ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::default::Default; // Import trait for Schema::default
use std::sync::Arc;
use tokio::sync::Mutex;

/// The name of the tool to validate an ad hoc GraphQL operation
pub const VALIDATE_TOOL_NAME: &str = "validate";

#[derive(Clone)]
pub struct Validate {
    pub tool: Tool,
    schema: Arc<Mutex<Valid<Schema>>>,
}

/// Input for the validate tool.
#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The GraphQL operation
    operation: String,
}

impl Validate {
    pub fn new(schema: Arc<Mutex<Valid<Schema>>>) -> Self {
        Self {
            schema,
            tool: Tool::new(
                VALIDATE_TOOL_NAME,
                "Validate a GraphQL operation against the schema. \
                Use the `introspect` tool to get information about the GraphQL schema. \
                Always use the schema to create operations - do not try arbitrary operations. \
                Operations should be validated prior to being executed.",
                schema_from_type!(Input),
            ),
        }
    }

    /// Validates the provided GraphQL query. Returns Ok(()) if valid, or an error if not.
    pub async fn execute(&self, input: Value) -> Result<CallToolResult, McpError> {
        let input = serde_json::from_value::<Input>(input).map_err(|_| {
            McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
        })?;

        operation_defs(&input.operation, true, None)
            .map_err(|e| McpError::new(ErrorCode::INVALID_PARAMS, e.to_string(), None))?
            .ok_or_else(|| {
                McpError::new(
                    ErrorCode::INVALID_PARAMS,
                    "Invalid operation type".to_string(),
                    None,
                )
            })?;

        let schema_guard = self.schema.lock().await;
        Parser::new()
            .parse_executable(&schema_guard, input.operation.as_str(), "operation.graphql")
            .map_err(|e| McpError::new(ErrorCode::INVALID_PARAMS, e.to_string(), None))?;
        Ok(CallToolResult {
            content: vec![Content::text("Operation is valid")],
            is_error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    static SCHEMA: std::sync::LazyLock<Arc<Mutex<Valid<Schema>>>> =
        std::sync::LazyLock::new(|| {
            Arc::new(Mutex::new(
                Schema::parse_and_validate("type Query { id: ID! }", "schema.graphql").unwrap(),
            ))
        });

    #[tokio::test]
    async fn validate_valid_query() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query Test { id }"
        });
        assert!(validate.execute(input).await.is_ok());
    }

    #[tokio::test]
    async fn validate_invalid_graphql_query() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query {"
        });
        assert!(validate.execute(input).await.is_err());
    }

    #[tokio::test]
    async fn validate_invalid_query_field() {
        let validate = Validate::new(SCHEMA.clone());
        let input = json!({
            "operation": "query { invalidField }"
        });
        assert!(validate.execute(input).await.is_err());
    }
}
