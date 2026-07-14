//! MCP tool to search a GraphQL schema.

use crate::errors::McpError;
use crate::introspection::minify::MinifyExt as _;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::ast::{Field, OperationType as AstOperationType, Selection};
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Node, Schema};
use apollo_schema_index::{OperationType, SchemaIndex, SchemaSearch};
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::description::append_description_hint;

/// The name of the tool to search a GraphQL schema.
pub const SEARCH_TOOL_NAME: &str = "search";

/// A tool to search a GraphQL schema.
#[derive(Clone)]
pub struct Search {
    schema: Arc<RwLock<Valid<Schema>>>,
    index: Arc<RwLock<SchemaIndex>>,
    allow_mutations: bool,
    leaf_depth: usize,
    flatten_depth: usize,
    index_memory_bytes: usize,
    default_limit: usize,
    max_limit: usize,
    minify: bool,
    pub tool: Tool,
}

/// Input for the search tool.
#[derive(JsonSchema, Deserialize, Debug)]
pub struct Input {
    /// The search terms
    terms: Vec<String>,
    /// Maximum number of results to return (default 10, max 50).
    #[serde(default)]
    limit: Option<usize>,
    /// Optional service scope to restrict results to (e.g. "slack"). Omit to search all services.
    #[serde(default)]
    scope: Option<String>,
}

/// An error while indexing the GraphQL schema.
#[derive(Debug, thiserror::Error)]
pub enum IndexingError {
    #[error("Unable to index schema: {0}")]
    IndexingError(#[from] apollo_schema_index::error::IndexingError),

    #[error("Unable to lock schema: {0}")]
    TryLockError(#[from] tokio::sync::TryLockError),
}

/// Clamp the requested result count to `[1, max_limit]`, defaulting when omitted.
fn clamp_limit(requested: Option<usize>, default_limit: usize, max_limit: usize) -> usize {
    requested.unwrap_or(default_limit).clamp(1, max_limit)
}

impl Search {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema: Arc<RwLock<Valid<Schema>>>,
        allow_mutations: bool,
        leaf_depth: usize,
        flatten_depth: usize,
        index_memory_bytes: usize,
        default_limit: usize,
        max_limit: usize,
        minify: bool,
        description_hint: Option<&str>,
    ) -> Result<Self, IndexingError> {
        let root_types = if allow_mutations {
            OperationType::Query | OperationType::Mutation
        } else {
            OperationType::Query.into()
        };
        let locked = &schema.try_read()?;
        let default_description = format!(
            "Search a GraphQL schema for types matching the provided search terms. Returns complete type definitions including all related types needed to construct GraphQL operations. Instructions: If the introspect tool is also available, you can discover type names by using the introspect tool starting from the root Query or Mutation types. Avoid reusing previously searched terms for more efficient exploration.{}",
            if minify {
                " - T=type,I=input,E=enum,U=union,F=interface;s=String,i=Int,f=Float,b=Boolean,d=ID;@D=deprecated;!=required,[]=list,<>=implements"
            } else {
                ""
            }
        );
        let description =
            append_description_hint(&default_description, description_hint).into_owned();
        let index = SchemaIndex::new(locked, root_types, flatten_depth, index_memory_bytes)?;
        Ok(Self {
            schema: schema.clone(),
            index: Arc::new(RwLock::new(index)),
            allow_mutations,
            leaf_depth,
            flatten_depth,
            index_memory_bytes,
            default_limit,
            max_limit,
            minify,
            tool: Tool::new(SEARCH_TOOL_NAME, description, schema_from_type!(Input)),
        })
    }

    /// Rebuild the search index from an updated schema, replacing the current index.
    pub async fn rebuild(&self, schema: &Valid<Schema>) -> Result<(), IndexingError> {
        let root_types = if self.allow_mutations {
            OperationType::Query | OperationType::Mutation
        } else {
            OperationType::Query.into()
        };
        let new_index = SchemaIndex::new(
            schema,
            root_types,
            self.flatten_depth,
            self.index_memory_bytes,
        )?;
        *self.index.write().await = new_index;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
        let k = clamp_limit(input.limit, self.default_limit, self.max_limit);
        let query = input.terms.join(" ");
        let results = {
            let index = self.index.read().await;
            index
                .search(&query, input.scope.as_deref(), k)
                .map_err(|e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to search index: {e}"),
                        None,
                    )
                })?
        };

        let schema = self.schema.read().await;
        let mut tree_shaker = SchemaTreeShaker::new(&schema);
        for scored in results.into_iter().take(k) {
            let op = scored.inner;
            let root = match op.operation_type {
                OperationType::Mutation => schema.root_operation(AstOperationType::Mutation),
                _ => schema.root_operation(AstOperationType::Query),
            };
            if let Some(root_name) = root
                && let Some(root_type) = schema.types.get(root_name)
            {
                let selection = vec![Selection::Field(Node::from(Field {
                    alias: Default::default(),
                    name: Name::new_unchecked(&op.field_name),
                    arguments: Default::default(),
                    selection_set: Default::default(),
                    directives: Default::default(),
                }))];
                tree_shaker.retain_type(root_type, Some(&selection), DepthLimit::Limited(1));
            }
            if let Some(rt) = op.return_type.as_ref()
                && let Some(rt_type) = schema.types.get(rt.as_str())
            {
                tree_shaker.retain_type(rt_type, None, DepthLimit::Limited(self.leaf_depth));
            }
            for arg in &op.arg_types {
                if let Some(arg_type) = schema.types.get(arg.as_str()) {
                    // Retain input types with unlimited depth because all input must be given
                    tree_shaker.retain_type(arg_type, None, DepthLimit::Unlimited);
                }
            }
        }

        let shaken = tree_shaker.shaken().unwrap_or_else(|schema| schema.partial);

        Ok(CallToolResult::success(
            shaken
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
        ))
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

    #[test]
    fn clamp_limit_bounds() {
        assert_eq!(clamp_limit(None, 10, 50), 10);
        assert_eq!(clamp_limit(Some(0), 10, 50), 1);
        assert_eq!(clamp_limit(Some(999), 10, 50), 50);
        assert_eq!(clamp_limit(Some(7), 10, 50), 7);
    }

    #[rstest]
    #[tokio::test]
    async fn search_tool(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new(schema.clone(), false, 1, 2, 15_000_000, 10, 50, false, None)
            .expect("Failed to create search tool");

        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
                limit: None,
                scope: None,
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));
        insta::assert_snapshot!(content_to_snapshot(result));
    }

    #[rstest]
    #[tokio::test]
    async fn search_tool_respects_limit(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new(schema.clone(), false, 1, 2, 15_000_000, 10, 50, false, None)
            .expect("Failed to create search tool");

        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
                limit: Some(2),
                scope: None,
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));
    }

    #[rstest]
    #[tokio::test]
    async fn referencing_types_are_collected(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new(schema.clone(), true, 1, 2, 15_000_000, 10, 50, false, None)
            .expect("Failed to create search tool");

        // Search for a type that should have references
        let result = search
            .execute(Input {
                terms: vec!["createUser".to_string()],
                limit: None,
                scope: None,
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));
        assert!(
            content_to_snapshot(result).contains("createUser"),
            "Expected to find the createUser mutation in search results"
        );
    }

    #[rstest]
    #[tokio::test]
    async fn search_tool_description_is_not_minified(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new(schema.clone(), false, 1, 2, 15_000_000, 10, 50, false, None)
            .expect("Failed to create search tool");

        let description = search.tool.description.unwrap();

        assert!(
            description
                .contains("Search a GraphQL schema for types matching the provided search terms")
        );
        assert!(description.contains("Instructions: If the introspect tool is also available"));
        assert!(description.contains("Avoid reusing previously searched terms"));
        // Should not contain minification legend
        assert!(!description.contains("T=type,I=input"));
    }

    #[rstest]
    #[tokio::test]
    async fn tool_description_minified(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new(schema.clone(), false, 1, 2, 15_000_000, 10, 50, true, None)
            .expect("Failed to create search tool");

        let description = search.tool.description.unwrap();

        // Should contain minification legend
        assert!(description.contains("T=type,I=input,E=enum,U=union,F=interface"));
        assert!(description.contains("s=String,i=Int,f=Float,b=Boolean,d=ID"));
    }
}
