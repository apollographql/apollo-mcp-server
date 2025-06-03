//! Tools to allow an AI agent to introspect a GraphQL schema and execute operations.

use crate::errors::McpError;
use crate::graphql;
use crate::operations::{MutationMode, operation_defs};
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use reqwest::header::{HeaderMap, HeaderValue};
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The name of the tool to execute an ad hoc GraphQL operation
pub(crate) const EXECUTE_TOOL_NAME: &str = "execute";

/// The name of the tool to get GraphQL schema type information
pub(crate) const INTROSPECT_TOOL_NAME: &str = "introspect";

/// The default depth to recurse the type hierarchy.
fn default_depth() -> u32 {
    1u32
}

/// A tool to get detailed information about specific types from the GraphQL schema.
#[derive(Clone)]
pub struct Introspect {
    schema: Arc<Mutex<Valid<Schema>>>,
    allow_mutations: bool,
    pub tool: Tool,
}

#[derive(JsonSchema, Deserialize)]
pub struct IntrospectInput {
    /// The name of the type to get information about.
    type_name: String,
    /// How far to recurse the type hierarchy. Use 0 for no limit. Defaults to 1.
    #[serde(default = "default_depth")]
    depth: u32,
}

impl Introspect {
    pub fn new(
        schema: Arc<Mutex<Valid<Schema>>>,
        root_query_type: Option<String>,
        root_mutation_type: Option<String>,
    ) -> Self {
        Self {
            schema,
            allow_mutations: root_mutation_type.is_some(),
            tool: Tool::new(
                INTROSPECT_TOOL_NAME,
                format!(
                    "Get detailed information about types from the GraphQL schema.{}{}",
                    root_query_type
                        .map(|t| format!(" Use the type name `{t}` to get root query fields."))
                        .unwrap_or_default(),
                    root_mutation_type
                        .map(|t| format!(" Use the type name `{t}` to get root mutation fields."))
                        .unwrap_or_default()
                ),
                schema_from_type!(IntrospectInput),
            ),
        }
    }

    pub async fn execute(&self, input: IntrospectInput) -> Result<CallToolResult, McpError> {
        let schema = self.schema.lock().await;
        let type_name = input.type_name.as_str();
        let mut tree_shaker = SchemaTreeShaker::new(&schema);
        match schema.types.get(type_name) {
            Some(extended_type) => tree_shaker.retain_type(
                extended_type,
                if input.depth > 0 {
                    DepthLimit::Limited(input.depth)
                } else {
                    DepthLimit::Unlimited
                },
            ),
            None => {
                return Ok(CallToolResult {
                    content: vec![],
                    is_error: None,
                });
            }
        }
        let shaken = tree_shaker.shaken().unwrap_or_else(|schema| schema.partial);

        Ok(CallToolResult {
            content: shaken
                .types
                .iter()
                .filter(|(_name, extended_type)| {
                    !extended_type.is_built_in()
                        && matches!(
                            extended_type,
                            ExtendedType::Object(_)
                                | ExtendedType::InputObject(_)
                                | ExtendedType::Scalar(_)
                                | ExtendedType::Enum(_)
                                | ExtendedType::Interface(_)
                                | ExtendedType::Union(_)
                        )
                        && schema
                            .root_operation(OperationType::Query)
                            .is_none_or(|root_name| {
                                extended_type.name() != root_name || type_name == root_name.as_str()
                            })
                        && schema
                            .root_operation(OperationType::Mutation)
                            .is_none_or(|root_name| {
                                extended_type.name() != root_name
                                    || (type_name == root_name.as_str() && self.allow_mutations)
                            })
                        && schema
                            .root_operation(OperationType::Subscription)
                            .is_none_or(|root_name| {
                                extended_type.name() != root_name || type_name == root_name.as_str()
                            })
                })
                .map(|(_, extended_type)| extended_type.serialize())
                .map(|serialized| serialized.to_string())
                .map(Content::text)
                .collect(),
            is_error: None,
        })
    }
}

#[derive(Clone)]
pub struct Execute {
    pub tool: Tool,
    mutation_mode: MutationMode,
}

#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The GraphQL operation
    query: String,

    /// The variable values
    variables: Option<Value>,
}

impl Execute {
    pub fn new(mutation_mode: MutationMode) -> Self {
        Self {
            mutation_mode,
            tool: Tool::new(
                EXECUTE_TOOL_NAME,
                "Execute a GraphQL operation. Use the `introspect` tool to get information about the GraphQL schema. Always use the schema to create operations - do not try arbitrary operations. DO NOT try to execute introspection queries.",
                schema_from_type!(Input),
            ),
        }
    }
}

impl graphql::Executable for Execute {
    fn persisted_query_id(&self) -> Option<String> {
        None
    }

    fn operation(&self, input: Value) -> Result<String, McpError> {
        let input = serde_json::from_value::<Input>(input).map_err(|_| {
            McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
        })?;

        // validate the operation
        operation_defs(&input.query, self.mutation_mode == MutationMode::All)
            .map_err(|e| McpError::new(ErrorCode::INVALID_PARAMS, e.to_string(), None))?;

        Ok(input.query)
    }

    fn variables(&self, input: Value) -> Result<Value, McpError> {
        let input = serde_json::from_value::<Input>(input).map_err(|_| {
            McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
        })?;
        match input.variables {
            None => Ok(Value::Null),
            Some(Value::Null) => Ok(Value::Null),
            Some(Value::String(s)) => serde_json::from_str(&s).map_err(|_| {
                McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
            }),
            Some(obj) if obj.is_object() => Ok(obj),
            _ => Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                "Invalid input".to_string(),
                None,
            )),
        }
    }

    fn headers(&self, default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue> {
        default_headers.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::Executable;
    use rmcp::serde_json::json;

    #[test]
    fn execute_query_with_variables_as_string() {
        let execute = Execute::new(MutationMode::None);

        let query = "query GetUser($id: ID!) { user(id: $id) { id name } }";
        let variables = json!({ "id": "123" });

        let input = json!({
            "query": query,
            "variables": variables.to_string()
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(query.to_string())
        );
        assert_eq!(Executable::variables(&execute, input), Ok(variables));
    }

    #[test]
    fn execute_query_with_variables_as_json() {
        let execute = Execute::new(MutationMode::None);

        let query = "query GetUser($id: ID!) { user(id: $id) { id name } }";
        let variables = json!({ "id": "123" });

        let input = json!({
            "query": query,
            "variables": variables
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(query.to_string())
        );
        assert_eq!(Executable::variables(&execute, input), Ok(variables));
    }

    #[test]
    fn execute_query_without_variables() {
        let execute = Execute::new(MutationMode::None);

        let query = "query GetUser($id: ID!) { user(id: $id) { id name } }";

        let input = json!({
            "query": query,
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(query.to_string())
        );
        assert_eq!(Executable::variables(&execute, input), Ok(Value::Null));
    }

    #[test]
    fn execute_query_invalid_input() {
        let execute = Execute::new(MutationMode::None);

        let input = json!({
            "nonsense": "whatever",
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                "Invalid input".to_string(),
                None
            ))
        );
        assert_eq!(
            Executable::variables(&execute, input),
            Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                "Invalid input".to_string(),
                None
            ))
        );
    }

    #[test]
    fn execute_query_invalid_variables() {
        let execute = Execute::new(MutationMode::None);

        let input = json!({
            "query": "query GetUser($id: ID!) { user(id: $id) { id name } }",
            "variables": "garbage",
        });

        assert_eq!(
            Executable::variables(&execute, input),
            Err(McpError::new(
                ErrorCode::INVALID_PARAMS,
                "Invalid input".to_string(),
                None
            ))
        );
    }
}
