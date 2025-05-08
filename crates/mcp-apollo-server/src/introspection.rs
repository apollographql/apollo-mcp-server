//! Tools to allow an AI agent to introspect a GraphQL schema and execute operations.

use crate::errors::McpError;
use crate::graphql;
use crate::operations::{MutationMode, operation_defs};
use crate::schema_from_type;
use crate::schema_tree_shake::SchemaTreeShaker;
use apollo_compiler::ast::OperationType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::{Value, json};
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The name of the tool to execute an ad hoc GraphQL operation
pub(crate) const EXECUTE_TOOL_NAME: &str = "execute";

/// The name of the tool to get GraphQL schema type information
pub(crate) const INTROSPECT_TOOL_NAME: &str = "introspect";

/// A schema can rename the root Query type, so this is used to indicate the root query type, whatever its name
pub(crate) const QUERY_TYPE_NAME: &str = "$Query";

/// A schema can rename the root Mutation type, so this is used to indicate the root mutation type, whatever its name
pub(crate) const MUTATION_TYPE_NAME: &str = "$Mutation";

/// The default depth to recurse the type hierarchy.
fn default_depth() -> u32 {
    1u32
}

/// A tool to get detailed information about specific types from the GraphQL schema.
#[derive(Clone)]
pub struct Introspect {
    mutation_mode: MutationMode,
    schema: Arc<Mutex<Valid<Schema>>>,
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
    pub fn new(schema: Arc<Mutex<Valid<Schema>>>, mutation_mode: MutationMode) -> Self {
        Self {
            mutation_mode,
            schema,
            tool: Tool::new(
                INTROSPECT_TOOL_NAME,
                format!(
                    "Get detailed information about specific types from the GraphQL schema. Use `{QUERY_TYPE_NAME}`{} to get available root fields - the `$` character is important.",
                    if mutation_mode == MutationMode::All {
                        format!(" or `{MUTATION_TYPE_NAME}`")
                    } else {
                        String::new()
                    }
                ),
                schema_from_type!(IntrospectInput),
            ),
        }
    }

    pub async fn execute(&self, input: IntrospectInput) -> Result<CallToolResult, McpError> {
        let schema = self.schema.lock().await;

        // Handle special cases for $Query and $Mutation
        let type_name = match if input.type_name.eq_ignore_ascii_case(QUERY_TYPE_NAME) {
            schema
                .root_operation(OperationType::Query)
                .map(Name::as_str)
        } else if input.type_name.eq_ignore_ascii_case(MUTATION_TYPE_NAME) {
            if self.mutation_mode == MutationMode::All {
                schema
                    .root_operation(OperationType::Mutation)
                    .map(Name::as_str)
            } else {
                None
            }
        } else {
            Some(input.type_name.as_str())
        } {
            Some(name) => name,
            None => {
                return Ok(CallToolResult {
                    content: vec![],
                    is_error: None,
                });
            }
        };

        let mut tree_shaker = SchemaTreeShaker::new(&schema);
        match schema.types.get(type_name) {
            Some(extended_type) => tree_shaker.retain_type(extended_type, None, input.depth),
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
                                extended_type.name() != root_name || type_name == root_name.as_str()
                            })
                        && schema
                            .root_operation(OperationType::Subscription)
                            .is_none_or(|root_name| extended_type.name() == root_name)
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
    variables: Option<String>,
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
            .map(|input| json!(input.variables))
            .map_err(|_| {
                McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None)
            })
    }
}
