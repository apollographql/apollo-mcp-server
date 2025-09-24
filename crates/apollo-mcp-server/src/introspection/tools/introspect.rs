use crate::errors::McpError;
use crate::introspection::minify::MinifyExt as _;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::schema::ExtendedType;
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
    minify: bool,
    pub tool: Tool,
}

/// Input for the introspect tool.
#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The name of the type to get information about.
    type_name: String,
    /// How far to recurse the type hierarchy. Use 0 for no limit. Defaults to 1.
    #[serde(default = "default_depth")]
    depth: usize,
}

impl Introspect {
    pub fn new(
        schema: Arc<Mutex<Valid<Schema>>>,
        root_query_type: Option<String>,
        root_mutation_type: Option<String>,
        minify: bool,
    ) -> Self {
        Self {
            schema,
            allow_mutations: root_mutation_type.is_some(),
            minify,
            tool: Tool::new(
                INTROSPECT_TOOL_NAME,
                tool_description(root_query_type, root_mutation_type, minify),
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
                None,
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
                    meta: None,
                    structured_content: None,
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
                        && schema
                            .root_operation(OperationType::Subscription)
                            .is_none_or(|root_name| extended_type.name() != root_name)
                })
                .map(|(_, extended_type)| extended_type)
                .map(|extended_type| self.serialize(extended_type))
                .map(Content::text)
                .collect(),
            is_error: None,
            meta: None,
            // The content being returned is a raw string, so no need to create structured content for it
            structured_content: None,
        })
    }

    fn serialize(&self, extended_type: &ExtendedType) -> String {
        if self.minify {
            extended_type.minify()
        } else {
            extended_type.serialize().to_string()
        }
    }
}

fn tool_description(
    root_query_type: Option<String>,
    root_mutation_type: Option<String>,
    minify: bool,
) -> String {
    if minify {
        "Get GraphQL type information - T=type,I=input,E=enum,U=union,F=interface;s=String,i=Int,f=Float,b=Boolean,d=ID;@D=deprecated;!=required,[]=list,<>=implements;".to_string()
    } else {
        format!(
            "Get information about a given GraphQL type defined in the schema. Instructions: Use this tool to explore the schema by providing specific type names. Start with the root query ({}) or mutation ({}) types to discover available fields. If the search tool is also available, use this tool first to get the fields, then use the search tool with relevant field return types and argument input types (ignore default GraphQL scalars) as search terms.",
            root_query_type.as_deref().unwrap_or("Query"),
            root_mutation_type.as_deref().unwrap_or("Mutation")
        )
    }
}

/// The default depth to recurse the type hierarchy.
fn default_depth() -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_compiler::Schema;
    use apollo_compiler::validation::Valid;
    use rstest::{fixture, rstest};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    const TEST_SCHEMA: &str = include_str!("testdata/schema.graphql");

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    #[rstest]
    #[tokio::test]
    async fn test_introspect_tool_description_is_not_minified(schema: Valid<Schema>) {
        let introspect = Introspect::new(Arc::new(Mutex::new(schema)), None, None, false);

        let description = introspect.tool.description.unwrap();

        assert!(
            description
                .contains("Get information about a given GraphQL type defined in the schema")
        );
        assert!(description.contains("Instructions: Use this tool to explore the schema"));
        // Should not contain minification legend
        assert!(!description.contains("T=type,I=input"));
        // Should mention conditional search tool usage
        assert!(description.contains("If the search tool is also available"));
    }

    #[rstest]
    #[tokio::test]
    async fn test_introspect_tool_description_is_minified_with_an_appropriate_legend(
        schema: Valid<Schema>,
    ) {
        let introspect = Introspect::new(Arc::new(Mutex::new(schema)), None, None, true);

        let description = introspect.tool.description.unwrap();

        // Should contain minification legend
        assert!(description.contains("T=type,I=input,E=enum,U=union,F=interface"));
        assert!(description.contains("s=String,i=Int,f=Float,b=Boolean,d=ID"));
    }
}
