//! MCP tool to search a GraphQL schema.

use crate::errors::McpError;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::Schema;
use apollo_compiler::ast::{NamedType, OperationType as AstOperationType};
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use enumset::{EnumSet, EnumSetType};
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::schema::document::Value as TantivyValue;
use tantivy::schema::*;
use tantivy::tokenizer::{Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{
    Index, TantivyError,
    schema::{STORED, Schema as TantivySchema},
};
use tokio::sync::Mutex;

/// The name of the tool to search a GraphQL schema.
pub const SEARCH_TOOL_NAME: &str = "search";

pub const TYPE_NAME_FIELD: &str = "type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const FIELDS_FIELD: &str = "fields";
pub const ROOT_PATH: &str = "root_path";

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

/// An error while indexing the GraphQL schema.
#[derive(Debug, thiserror::Error)]
pub enum IndexingError {
    #[error("Unable to index schema: {0}")]
    TantivyError(#[from] TantivyError),

    #[error("Unable to lock schema: {0}")]
    TryLockError(#[from] tokio::sync::TryLockError),
}

/// Redefine operation type to support enum sets
#[derive(EnumSetType, Debug)]
pub enum OperationType {
    Query,
    Mutation,
    Subscription,
}

impl From<AstOperationType> for OperationType {
    fn from(value: AstOperationType) -> Self {
        match value {
            AstOperationType::Query => OperationType::Query,
            AstOperationType::Mutation => OperationType::Mutation,
            AstOperationType::Subscription => OperationType::Subscription,
        }
    }
}

impl From<OperationType> for AstOperationType {
    fn from(value: OperationType) -> Self {
        match value {
            OperationType::Query => AstOperationType::Query,
            OperationType::Mutation => AstOperationType::Mutation,
            OperationType::Subscription => AstOperationType::Subscription,
        }
    }
}

/// A path from a root operation to a type in the schema
#[derive(Clone, Deserialize, Serialize, PartialEq)]
struct Path<'a> {
    types: Vec<Cow<'a, NamedType>>,
}

impl<'a> Path<'a> {
    fn new(types: Vec<&'a NamedType>) -> Self {
        Self {
            types: types.into_iter().map(Cow::Borrowed).collect(),
        }
    }

    fn extend(&self, next_type: &'a NamedType) -> Self {
        let mut types = self.types.clone();
        types.push(Cow::Borrowed(next_type));
        Self { types }
    }

    fn has_cycle(&self) -> bool {
        if let Some(last_type) = self.types.last() {
            self.types
                .get(0..self.types.len() - 1)
                .map(|slice| slice.contains(last_type))
                .unwrap_or(false)
        } else {
            false
        }
    }
}

impl<'a> fmt::Display for Path<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.types
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        )
    }
}

/// An extension trait to add type traversal to a schema
trait SchemaExt {
    /// Traverse the type hierarchy in the schema in depth-first order, starting with the given root types
    fn traverse(
        &self,
        root_types: EnumSet<OperationType>,
    ) -> impl Iterator<Item = (&ExtendedType, Path)>;
}

impl SchemaExt for Schema {
    /// Iterate the types in the schema through a depth-first traversal starting with the given root operation types
    fn traverse(
        &self,
        root_types: EnumSet<OperationType>,
    ) -> impl Iterator<Item = (&ExtendedType, Path)> {
        let mut stack = vec![];

        // The unique paths to each type
        let mut paths: HashMap<&NamedType, Vec<Path>> = HashMap::default();

        root_types
            .iter()
            .filter_map(|root_type| self.root_operation(root_type.into()))
            .for_each(|root_type| {
                stack.push((root_type, Path::new(vec![root_type])));
            });

        std::iter::from_fn(move || {
            while let Some((named_type, current_path)) = stack.pop() {
                // Skip if we've found a cycle
                if current_path.has_cycle() {
                    continue;
                }

                // Skip if we've visited this path before
                if let Some(paths) = paths.get(named_type) {
                    if paths.contains(&current_path) {
                        continue;
                    }
                }

                paths
                    .entry(named_type)
                    .or_default()
                    .push(current_path.clone());

                if let Some(extended_type) = self.types.get(named_type) {
                    if !extended_type.is_built_in() {
                        match extended_type {
                            ExtendedType::Object(obj) => {
                                stack.extend(
                                    obj.fields
                                        .values()
                                        .map(|field| &field.ty)
                                        .map(|ty| ty.inner_named_type())
                                        .map(|next_type| {
                                            (next_type, current_path.extend(next_type))
                                        }),
                                );
                            }
                            ExtendedType::Interface(interface) => {
                                stack.extend(
                                    interface
                                        .fields
                                        .values()
                                        .map(|field| &field.ty)
                                        .map(|ty| ty.inner_named_type())
                                        .map(|next_type| {
                                            (next_type, current_path.extend(next_type))
                                        }),
                                );
                            }
                            ExtendedType::InputObject(input) => {
                                stack.extend(
                                    input
                                        .fields
                                        .values()
                                        .map(|field| &field.ty)
                                        .map(|ty| ty.inner_named_type())
                                        .map(|next_type| {
                                            (next_type, current_path.extend(next_type))
                                        }),
                                );
                            }
                            ExtendedType::Enum(enum_type) => {
                                stack.extend(
                                    enum_type.values.iter().map(|(_, value)| &value.value).map(
                                        |next_type| (next_type, current_path.extend(next_type)),
                                    ),
                                );
                            }
                            ExtendedType::Union(union) => {
                                stack.extend(
                                    union.members.iter().map(|member| &member.name).map(
                                        |next_type| (next_type, current_path.extend(next_type)),
                                    ),
                                );
                            }
                            _ => {}
                        }
                        return Some((extended_type, current_path));
                    }
                }
            }
            None
        })
    }
}

/// Index a schema for searching.
fn index(schema: Arc<Mutex<Valid<Schema>>>) -> Result<Index, IndexingError> {
    let start_time = Instant::now();

    // Register a custom analyzer with English stemming and lowercasing
    let stem_analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(Stemmer::new(Language::English))
        .build();

    // Create the schema builder and add fields with the custom analyzer
    let mut index_schema = TantivySchema::builder();
    let type_name_field = index_schema.add_text_field(
        TYPE_NAME_FIELD,
        TextOptions::default()
            .set_indexing_options(TextFieldIndexing::default().set_tokenizer("en_stem"))
            .set_stored(),
    );
    let description_field = index_schema.add_text_field(
        DESCRIPTION_FIELD,
        TextOptions::default()
            .set_indexing_options(TextFieldIndexing::default().set_tokenizer("en_stem"))
            .set_stored(),
    );
    let fields_field = index_schema.add_text_field(
        FIELDS_FIELD,
        TextOptions::default()
            .set_indexing_options(TextFieldIndexing::default().set_tokenizer("en_stem"))
            .set_stored(),
    );
    let root_path_field = index_schema.add_text_field(ROOT_PATH, STORED);
    let index_schema = index_schema.build();

    let index = Index::create_in_ram(index_schema);
    index.tokenizers().register("en_stem", stem_analyzer);

    let mut index_writer = index.writer(INDEX_MEMORY_BYTES)?;

    let schema = schema.try_lock()?;

    let mut i = 0usize;
    for (extended_type, path) in schema.traverse(OperationType::Query | OperationType::Mutation) {
        i += 1;
        // if i % 1000 == 0 {
        //     println!("Indexed {} paths", i);
        // }
        // println!("Indexing {}", path);
        // TODO: indexing ALL the paths takes forever on a big schema. Need a more efficient algorithm.
        let mut doc = TantivyDocument::default();
        doc.add_text(type_name_field, extended_type.name());
        doc.add_text(
            description_field,
            extended_type
                .description()
                .map(|d| d.to_string())
                .unwrap_or(String::from("")),
        );
        doc.add_text(
            root_path_field,
            serde_json::to_string(&path).map_err(|e| {
                IndexingError::TantivyError(TantivyError::InvalidArgument(format!(
                    // TODO: better error here
                    "Failed to serialize path: {}",
                    e
                )))
            })?,
        );

        // TODO: index field parameters
        // TODO: index documentation descriptions
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
        index_writer.add_document(doc)?;
    }
    index_writer.commit()?;

    let elapsed = start_time.elapsed();
    println!("Indexed {} paths in {:.2?}", i, elapsed);

    Ok(index)
}

impl Search {
    pub fn new(
        schema: Arc<Mutex<Valid<Schema>>>,
        allow_mutations: bool,
    ) -> Result<Self, IndexingError> {
        Ok(Self {
            schema: schema.clone(),
            index: index(schema)?,
            allow_mutations,
            tool: Tool::new(
                SEARCH_TOOL_NAME,
                "Search a GraphQL schema",
                schema_from_type!(Input),
            ),
        })
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

        let schema = self.schema.lock().await;
        let mut tree_shaker = SchemaTreeShaker::new(&schema);

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

            // TODO: rank by shortest path to root, and only return the top few paths
            // TODO: limit the total size of the output?
            let top_docs = searcher
                .search(&query, &TopDocs::with_limit(100))
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

                let root_path = doc
                    .get_first(self.index.schema().get_field(ROOT_PATH).map_err(|e| {
                        McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            format!("Failed to get {ROOT_PATH} field: {}", e),
                            None,
                        )
                    })?)
                    .and_then(|v| serde_json::from_str::<Path>(v.as_str().unwrap_or_default()).ok()) // TODO: error handling on as_str
                    .ok_or_else(|| {
                        McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            String::from("Failed to deserialize root path"),
                            None,
                        )
                    })?;

                tracing::info!("matching root path: {root_path}");

                // TODO: add fields to the path so we can include just the relevant fields for non-leaf types?
                let types = root_path.types;
                let path_len = types.len();
                for (i, type_name) in types.into_iter().enumerate() {
                    if let Some(extended_type) = schema.types.get(type_name.as_str()) {
                        let depth = if i == path_len - 1 {
                            // TODO: maybe don't include more leaf type info if it would go over a size limit?
                            DepthLimit::Limited(2)
                        } else {
                            DepthLimit::Limited(1)
                        };
                        tree_shaker.retain_type(extended_type, depth)
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
                .map(|(_, extended_type)| extended_type.serialize())
                .map(|serialized| serialized.to_string())
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

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    #[rstest]
    fn test_schema_traverse(schema: Valid<Schema>) {
        let mut paths = vec![];
        for (_extended_type, path) in schema
            .traverse(OperationType::Query | OperationType::Mutation | OperationType::Subscription)
        {
            paths.push(path.to_string());
        }
        insta::assert_debug_snapshot!(paths);
    }

    #[rstest]
    #[tokio::test]
    async fn test_search_tool(schema: Valid<Schema>) {
        let schema = Arc::new(Mutex::new(schema));
        let search = Search::new(schema.clone(), true).expect("Failed to create search tool");

        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));

        let content = result
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
            .join("\n");

        insta::assert_snapshot!(content);
    }
}
