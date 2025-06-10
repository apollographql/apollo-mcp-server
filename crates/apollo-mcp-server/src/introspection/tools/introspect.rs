use crate::errors::McpError;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::validation::Valid;
use rmcp::model::{CallToolResult, Content, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The name of the tool to get GraphQL schema type information
pub const INTROSPECT_TOOL_NAME: &str = "introspect";

/// A tool to get detailed information about specific types from the GraphQL schema.
#[derive(Clone)]
pub struct Introspect {
    schema: Arc<Mutex<Valid<Schema>>>,
    allow_mutations: bool,
    pub tool: Tool,
}

/// Input for the introspect tool.
#[derive(JsonSchema, Deserialize)]
pub struct Input {
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
                schema_from_type!(Input),
            ),
        }
    }

    pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
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
                        && schema
                            .root_operation(OperationType::Mutation)
                            .is_none_or(|root_name| {
                                extended_type.name() != root_name
                                    || (type_name == root_name.as_str() && self.allow_mutations)
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

/// The default depth to recurse the type hierarchy.
fn default_depth() -> u32 {
    1u32
}
