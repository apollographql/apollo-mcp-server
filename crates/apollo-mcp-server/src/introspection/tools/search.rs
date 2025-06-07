//! MCP tool to search a GraphQL schema.

use crate::errors::McpError;
use crate::schema_from_type;
use apollo_compiler::Schema;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::sync::Arc;
use apollo_compiler::ast::OperationType;
use tantivy::schema::document::Value as TantivyValue;
use tantivy::schema::*;
use tantivy::{
    Index,
    schema::{STORED, Schema as TantivySchema},
};
use tokio::sync::Mutex;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};

/// The name of the tool to search a GraphQL schema.
pub const SEARCH_TOOL_NAME: &str = "search";

pub const TYPE_NAME_FIELD: &str = "type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const FIELDS_FIELD: &str = "fields";
pub const INDEX_MEMORY_BYTES: usize = 50_000_000;

/// A tool to search a GraphQL schema.
#[derive(Clone)]
pub struct Search {
    schema: Arc<Mutex<Valid<Schema>>>,
    index: Index,
    allow_mutations: bool,
    pub tool: Tool,
}

/// Input for the search tool.
#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The search terms
    terms: Vec<String>,
}

#[allow(clippy::unwrap_used)] // TODO: error handling
fn index(graphql_schema: Arc<Mutex<Valid<Schema>>>) -> Index {
    let mut index_schema = TantivySchema::builder();
    let type_name_field = index_schema.add_text_field(TYPE_NAME_FIELD, TEXT | STORED);
    let description_field = index_schema.add_text_field(DESCRIPTION_FIELD, TEXT | STORED);
    let fields_field = index_schema.add_text_field(FIELDS_FIELD, TEXT | STORED);
    let index_schema = index_schema.build();

    let index = Index::create_in_ram(index_schema);
    let mut index_writer = index.writer(INDEX_MEMORY_BYTES).unwrap(); // TODO: error handling

    // TODO: recurse down the schema types to build up paths to root for each matching field
    let graphql_schema = graphql_schema.try_lock().unwrap(); // TODO: error handling
    for (type_name, extended_type) in &graphql_schema.types {
        if !extended_type.is_built_in() {
            let mut doc = TantivyDocument::default();

            // Add type name
            doc.add_text(type_name_field, type_name);

            // Add description if available
            if let Some(description) = extended_type.description() {
                doc.add_text(description_field, description);
            }

            // Add fields
            // TODO: index field parameters
            let fields = match extended_type {
                ExtendedType::Object(obj) => obj
                    .fields
                    .iter()
                    .map(|(name, field)| format!("{}: {}", name, field.ty.inner_named_type()))
                    .collect::<Vec<_>>()
                    .join(", "),
                ExtendedType::Interface(interface) => interface
                    .fields
                    .iter()
                    .map(|(name, field)| format!("{}: {}", name, field.ty.inner_named_type()))
                    .collect::<Vec<_>>()
                    .join(", "),
                ExtendedType::InputObject(input) => input
                    .fields
                    .iter()
                    .map(|(name, field)| format!("{}: {}", name, field.ty.inner_named_type()))
                    .collect::<Vec<_>>()
                    .join(", "),
                ExtendedType::Enum(enum_type) => format!(
                    "{}: {}",
                    enum_type.name,
                    enum_type
                        .values
                        .iter()
                        .map(|(name, _)| name.to_string())
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                _ => String::new(),
            };
            doc.add_text(fields_field, &fields);

            index_writer.add_document(doc).unwrap();
        }
    }
    index_writer.commit().unwrap();
    index
}

impl Search {
    pub fn new(schema: Arc<Mutex<Valid<Schema>>>, allow_mutations: bool) -> Self {
        Self {
            schema: schema.clone(),
            index: index(schema),
            allow_mutations,
            tool: Tool::new(
                SEARCH_TOOL_NAME,
                "Search a GraphQL schema",
                schema_from_type!(Input),
            ),
        }
    }

    pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
        let reader = self.index.reader().map_err(|e| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to create index reader: {}", e),
                None,
            )
        })?;
        let searcher = reader.searcher();

        let mut type_names = Vec::new();

        for term in input.terms {
            let query = tantivy::query::QueryParser::for_index(
                &self.index,
                vec![
                    self.index
                        .schema()
                        .get_field(TYPE_NAME_FIELD)
                        .map_err(|e| {
                            McpError::new(
                                ErrorCode::INTERNAL_ERROR,
                                format!("Failed to get type_name field: {}", e),
                                None,
                            )
                        })?,
                    self.index
                        .schema()
                        .get_field(DESCRIPTION_FIELD)
                        .map_err(|e| {
                            McpError::new(
                                ErrorCode::INTERNAL_ERROR,
                                format!("Failed to get description field: {}", e),
                                None,
                            )
                        })?,
                    self.index.schema().get_field(FIELDS_FIELD).map_err(|e| {
                        McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            format!("Failed to get fields field: {}", e),
                            None,
                        )
                    })?,
                ],
            )
            .parse_query(&term)
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to parse query: {}", e),
                    None,
                )
            })?;

            let top_docs = searcher
                .search(&query, &tantivy::collector::TopDocs::with_limit(10))
                .map_err(|e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to search: {}", e),
                        None,
                    )
                })?;

            for (_, doc_address) in top_docs {
                let doc: TantivyDocument = searcher.doc(doc_address).map_err(|e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to get document: {}", e),
                        None,
                    )
                })?;

                let type_name = doc
                    .get_first(
                        self.index
                            .schema()
                            .get_field(TYPE_NAME_FIELD)
                            .map_err(|e| {
                                McpError::new(
                                    ErrorCode::INTERNAL_ERROR,
                                    format!("Failed to get type_name field: {}", e),
                                    None,
                                )
                            })?,
                    )
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                type_names.push(type_name.to_string());
            }
        }

        let schema = self.schema.lock().await;
        let mut tree_shaker = SchemaTreeShaker::new(&schema);
        for type_name in type_names {
            if let Some(extended_type) = schema.types.get(type_name.as_str()) {
                tree_shaker.retain_type(
                    extended_type,
                    DepthLimit::Limited(1),
                )
            }
        }
        let shaken = tree_shaker.shaken().unwrap_or_else(|schema| schema.partial);

        Ok(CallToolResult {
            content: shaken
                .types
                .iter()
                .filter(|(_name, extended_type)| {
                    !extended_type.is_built_in() &&
                        schema
                            .root_operation(OperationType::Mutation)
                            .is_none_or(|root_name| {
                                extended_type.name() != root_name || self.allow_mutations
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
