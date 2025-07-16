//! MCP tool to search a GraphQL schema.

use crate::errors::McpError;
use crate::introspection::minify::MinifyExt as _;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::ast::{Field, OperationType as AstOperationType, Selection};
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Node, Schema};
use apollo_schema_index::{OperationType, Options, SchemaIndex};
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// The name of the tool to search a GraphQL schema.
pub const SEARCH_TOOL_NAME: &str = "search";

/// The maximum number of search results to consider.
const MAX_SEARCH_RESULTS: usize = 5;

/// A tool to search a GraphQL schema.
#[derive(Clone)]
pub struct Search {
    schema: Arc<Mutex<Valid<Schema>>>,
    index: SchemaIndex,
    allow_mutations: bool,
    leaf_depth: usize,
    minify: bool,
    pub tool: Tool,
}

/// Input for the search tool.
#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The search terms
    terms: Vec<String>,
}

/// An error while indexing the GraphQL schema.
#[derive(Debug, thiserror::Error)]
pub enum IndexingError {
    #[error("Unable to index schema: {0}")]
    IndexingError(#[from] apollo_schema_index::error::IndexingError),

    #[error("Unable to lock schema: {0}")]
    TryLockError(#[from] tokio::sync::TryLockError),
}

impl Search {
    pub fn new(
        schema: Arc<Mutex<Valid<Schema>>>,
        allow_mutations: bool,
        leaf_depth: usize,
        index_memory_bytes: usize,
        minify: bool,
    ) -> Result<Self, IndexingError> {
        let root_types = if allow_mutations {
            OperationType::Query | OperationType::Mutation
        } else {
            OperationType::Query.into()
        };
        let locked = &schema.try_lock()?;
        Ok(Self {
            schema: schema.clone(),
            index: SchemaIndex::new(locked, root_types, index_memory_bytes)?,
            allow_mutations,
            leaf_depth,
            minify,
            tool: Tool::new(
                SEARCH_TOOL_NAME,
                format!("Search a GraphQL schema{}",
                    if minify {
                        " - T=type,I=input,E=enum,U=union,F=interface;s=String,i=Int,f=Float,b=Boolean,d=ID;!=required,[]=list,<>=implements"
                    } else {
                        ""
                    }),
                schema_from_type!(Input),
            ),
        })
    }

    pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
        let mut root_paths = self
            .index
            .search(input.terms.clone(), Options::default())
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to search index: {e}"),
                    None,
                )
            })?;

        root_paths.truncate(MAX_SEARCH_RESULTS);
        debug!(
            "Root paths for search terms: {}\n{}",
            input.terms.join(", "),
            root_paths
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>()
                .join("\n"),
        );

        let schema = self.schema.lock().await;
        let mut tree_shaker = SchemaTreeShaker::new(&schema);
        for root_path in root_paths {
            let path_len = root_path.inner.len();
            for (i, path_node) in root_path.inner.into_iter().enumerate() {
                if let Some(extended_type) = schema.types.get(path_node.node_type.as_str()) {
                    let (selection_set, depth) = if i == path_len - 1 {
                        (None, DepthLimit::Limited(self.leaf_depth))
                    } else {
                        (
                            path_node.field_name.as_ref().map(|field_name| {
                                vec![Selection::Field(Node::from(Field {
                                    alias: Default::default(),
                                    name: Name::new_unchecked(field_name),
                                    arguments: Default::default(),
                                    selection_set: Default::default(),
                                    directives: Default::default(),
                                }))]
                            }),
                            DepthLimit::Limited(1),
                        )
                    };
                    tree_shaker.retain_type(extended_type, selection_set.as_ref(), depth)
                }
                for field_arg in path_node.field_args {
                    if let Some(extended_type) = schema.types.get(field_arg.as_str()) {
                        // Retain input types with unlimited depth because all input must be given
                        tree_shaker.retain_type(extended_type, None, DepthLimit::Unlimited);
                    }
                }
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
                            .root_operation(AstOperationType::Mutation)
                            .is_none_or(|root_name| {
                                extended_type.name() != root_name || self.allow_mutations
                            })
                })
                .map(|(_, extended_type)| {
                    if self.minify {
                        extended_type.minify()
                    } else {
                        extended_type.serialize().to_string()
                    }
                })
                .map(Content::text)
                .collect(),
            is_error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::RawContent;
    use rstest::{fixture, rstest};
    use std::ops::Deref;

    const TEST_SCHEMA: &str = include_str!("testdata/schema.graphql");

    fn content_to_snapshot(result: CallToolResult) -> String {
        result
            .content
            .into_iter()
            .filter_map(|c| {
                let c = c.deref();
                match c {
                    RawContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                }
            })
            .collect::<Vec<String>>()
            .join("\n")
    }

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    #[rstest]
    #[tokio::test]
    async fn test_search_tool(schema: Valid<Schema>) {
        let schema = Arc::new(Mutex::new(schema));
        let search = Search::new(schema.clone(), false, 1, 15_000_000, false)
            .expect("Failed to create search tool");

        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));
        insta::assert_snapshot!(content_to_snapshot(result));
    }

    #[rstest]
    #[tokio::test]
    async fn test_referencing_types_are_collected(schema: Valid<Schema>) {
        let schema = Arc::new(Mutex::new(schema));
        let search =
            Search::new(schema.clone(), true, 1, 15_000_000, false).expect("Failed to create search tool");

        // Search for a type that should have references
        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));
        assert!(
            content_to_snapshot(result).contains("createUser"),
            "Expected to find the createUser mutation in search results"
        );
    }
}
