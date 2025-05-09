//! Tools to allow an AI agent to introspect a GraphQL schema and execute operations.

use crate::errors::McpError;
use crate::graphql;
use crate::operations::{MutationMode, operation_defs};
use crate::schema_from_type;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use rmcp::model::{ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) const GET_SCHEMA_TOOL_NAME: &str = "schema";
pub(crate) const EXECUTE_TOOL_NAME: &str = "execute";

#[derive(Clone)]
pub struct GetSchema {
    pub schema: Arc<Mutex<Valid<Schema>>>,
    pub tool: Tool,
}

#[derive(JsonSchema, Deserialize)]
pub struct GetSchemaInput {}

impl GetSchema {
    pub fn new(
        schema: Arc<Mutex<Valid<Schema>>>,
        execute_introspection: bool,
        enable_explorer: bool,
    ) -> Self {
        Self {
            schema,
            tool: Tool::new(
                GET_SCHEMA_TOOL_NAME,
                if execute_introspection && enable_explorer {
                    "Get the GraphQL schema. Operations on this schema can be executed using the `execute` tool or used with the explorer tool."
                } else if execute_introspection {
                    "Get the GraphQL schema. Operations on this schema can be executed using the `execute` tool."
                } else if enable_explorer {
                    "Get the GraphQL schema. Operations on this schema can be used with the explorer tool."
                } else {
                    "Get the GraphQL schema. Operations on this schema can be displayed to the user."
                },
                schema_from_type!(GetSchemaInput),
            ),
        }
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
    variables: Option<String>,
}

impl Execute {
    pub fn new(mutation_mode: MutationMode) -> Self {
        Self {
            mutation_mode,
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
        let input = serde_json::from_value::<Input>(input).map_err(|_| {
            McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
        })?;

        // validate the operation
        operation_defs(
            &input.query,
            self.mutation_mode == MutationMode::All,
            self.mutation_mode,
        )
        .map_err(|e| McpError::new(ErrorCode::INVALID_PARAMS, e.to_string(), None))?;

        Ok(input.query)
    }

    fn variables(&self, input: Value) -> Result<Value, McpError> {
        serde_json::from_value::<Input>(input)
            .map(|input| serde_json::json!(input.variables))
            .map_err(|_| {
                McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
            })
    }
}
