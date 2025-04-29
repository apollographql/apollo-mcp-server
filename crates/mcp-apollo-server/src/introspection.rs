//! Tools to allow an AI agent to introspect a GraphQL schema and execute operations.

use crate::errors::McpError;
use crate::graphql;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use rmcp::model::{ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;

pub(crate) const GET_SCHEMA_TOOL_NAME: &str = "schema";
pub(crate) const EXECUTE_TOOL_NAME: &str = "execute";

macro_rules! schema_from_type {
    ($type:ty) => {{
        match serde_json::to_value(schemars::schema_for!($type)) {
            Ok(Value::Object(schema)) => schema,
            _ => panic!("Failed to generate schema for {}", stringify!($type)),
        }
    }};
}

#[derive(Clone)]
pub struct GetSchema {
    pub schema: Valid<Schema>,
    pub tool: Tool,
}

#[derive(JsonSchema, Deserialize)]
pub struct GetSchemaInput {}

impl GetSchema {
    pub fn new(schema: Valid<Schema>) -> Self {
        Self {
            schema,
            tool: Tool::new(
                GET_SCHEMA_TOOL_NAME,
                "Get the GraphQL schema. Operations on this schema can be executed using the `execute` tool.",
                schema_from_type!(GetSchemaInput),
            ),
        }
    }
}

#[derive(Clone)]
pub struct Execute {
    pub tool: Tool,
}

#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The GraphQL operation
    query: String,

    /// The variable values
    variables: Option<String>,
}

impl Execute {
    pub fn new() -> Self {
        Self {
            tool: Tool::new(
                EXECUTE_TOOL_NAME,
                "Execute a GraphQL operation. Use the `schema` tool to get the GraphQL schema. Always use the schema to create operations - do not try arbitrary operations. DO NOT try to execute introspection queries.",
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
        serde_json::from_value::<Input>(input)
            .map(|input| input.query)
            .map_err(|_| {
                McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
            })
    }

    fn variables(&self, input: Value) -> Result<Value, McpError> {
        serde_json::from_value::<Input>(input)
            .map(|input| serde_json::json!(input.variables))
            .map_err(|_| {
                McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
            })
    }
}
