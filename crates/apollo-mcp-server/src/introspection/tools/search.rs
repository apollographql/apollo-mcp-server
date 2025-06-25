//! MCP tool to search a GraphQL schema.

use crate::errors::McpError;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::Schema;
use apollo_compiler::ast::{NamedType, OperationType as AstOperationType};
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use enumset::{EnumSet, EnumSetType};
use itertools::Itertools;
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::sync::Arc;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::document::Value as TantivyValue;
use tantivy::schema::*;
use tantivy::tokenizer::{Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{
    Index, TantivyError,
    schema::{STORED, Schema as TantivySchema},
};
use tokio::sync::Mutex;
use tracing::{debug, info};

/// The name of the tool to search a GraphQL schema.
pub const SEARCH_TOOL_NAME: &str = "search";

pub const TYPE_NAME_FIELD: &str = "type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const FIELDS_FIELD: &str = "fields";
pub const RAW_TYPE_NAME_FIELD: &str = "raw_type_name";
pub const REFERENCING_TYPES_FIELD: &str = "referencing_types";

pub const INDEX_MEMORY_BYTES: usize = 50_000_000;

const MAX_ROOT_PATHS: usize = 3;

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
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
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

    fn referencing_type(&self) -> Option<&NamedType> {
        if self.types.len() > 1 {
            self.types.get(self.types.len() - 2).map(|t| t.as_ref())
        } else {
            None
        }
    }
}

impl<'a> Display for Path<'a> {
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

struct ScoredPath {
    path: Path<'static>,
    score: f32,
}

impl ScoredPath {
    fn new(path: Path<'static>, score: f32) -> Self {
        Self { path, score }
    }

    fn score(&self) -> f32 {
        self.score
    }
}

impl PartialEq for ScoredPath {
    fn eq(&self, other: &Self) -> bool {
        self.score() == other.score()
    }
}

impl Eq for ScoredPath {}

impl PartialOrd for ScoredPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

impl Ord for ScoredPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score().total_cmp(&other.score())
    }
}

impl Hash for ScoredPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

impl Display for ScoredPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.path, self.score)
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

        let mut references: HashMap<&NamedType, Vec<NamedType>> = HashMap::default();

        root_types
            .iter()
            .rev() // Reverse so root types are traversed in the order specified
            .filter_map(|root_type| self.root_operation(root_type.into()))
            .for_each(|root_type| {
                stack.push((root_type, Path::new(vec![root_type])));
            });

        std::iter::from_fn(move || {
            while let Some((named_type, current_path)) = stack.pop() {
                if current_path.has_cycle() {
                    continue;
                }
                let references = references.entry(named_type);

                let traverse_children: bool = matches!(references, Entry::Vacant(_));

                references.or_insert(
                    current_path
                        .referencing_type()
                        .map(|t| vec![t.clone()])
                        .unwrap_or_default(),
                );

                if let Some(extended_type) = self.types.get(named_type) {
                    if !extended_type.is_built_in() {
                        if traverse_children {
                            match extended_type {
                                ExtendedType::Object(obj) => {
                                    stack.extend(
                                        obj.fields
                                            .values()
                                            .map(|field| &field.ty)
                                            .map(|ty| ty.inner_named_type())
                                            .unique()
                                            .map(|next_type| {
                                                (next_type, current_path.extend(next_type))
                                            }),
                                    );
                                    stack.extend(
                                        obj.fields
                                            .values()
                                            .flat_map(|field| &field.arguments)
                                            .map(|arg| arg.ty.inner_named_type())
                                            .unique()
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
                                            .unique()
                                            .map(|next_type| {
                                                (next_type, current_path.extend(next_type))
                                            }),
                                    );
                                    stack.extend(
                                        interface.fields
                                            .values()
                                            .flat_map(|field| &field.arguments)
                                            .map(|arg| arg.ty.inner_named_type())
                                            .unique()
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
                                            .unique()
                                            .map(|next_type| {
                                                (next_type, current_path.extend(next_type))
                                            }),
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
#[allow(clippy::unwrap_used)]
fn index(
    schema: Arc<Mutex<Valid<Schema>>>,
    include_mutations: bool,
) -> Result<Index, IndexingError> {
    let start_time = Instant::now();

    // Register a custom analyzer with English stemming and lowercasing
    // TODO: support other languages
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
    let raw_type_name_field = index_schema.add_text_field(
        RAW_TYPE_NAME_FIELD,
        TextOptions::default()
            .set_indexing_options(TextFieldIndexing::default().set_tokenizer("raw"))
            .set_stored(),
    ); // indexed as as exact name (no stemming or lowercasing)
    let referencing_types_field = index_schema.add_text_field(REFERENCING_TYPES_FIELD, STORED); // not indexed
    let index_schema = index_schema.build();

    let index = Index::create_in_ram(index_schema);
    index.tokenizers().register("en_stem", stem_analyzer);

    let mut index_writer = index.writer(INDEX_MEMORY_BYTES)?;

    let schema = schema.try_lock()?;

    // Collect all referencing types for each type
    let mut type_references: HashMap<String, Vec<String>> = HashMap::default();

    let root_types = if include_mutations {
        OperationType::Query | OperationType::Mutation
    } else {
        EnumSet::only(OperationType::Query)
    };

    for (extended_type, path) in schema.traverse(root_types) {
        let entry = type_references
            .entry(extended_type.name().to_string())
            .or_default();
        if let Some(ref_type) = path.referencing_type() {
            entry.push(ref_type.to_string());
        }
    }

    // Debug: Print the collected references
    for (type_name, references) in &type_references {
        debug!("Type '{}' is referenced by: {:?}", type_name, references);
    }

    // Create one document per type
    for (type_name, references) in &type_references {
        let type_name = NamedType::new_unchecked(type_name.as_str());
        let extended_type = schema.types.get(&type_name).unwrap(); // TODO - we already have the extended type above, can store and avoid this lookup
        // TODO: include these?
        // if extended_type.is_built_in() {
        //     println!("Skipping built-in type: {}", extended_type.name());
        //     continue;
        // }

        let mut doc = TantivyDocument::default();
        doc.add_text(type_name_field, extended_type.name());
        doc.add_text(raw_type_name_field, extended_type.name());
        doc.add_text(
            description_field,
            extended_type
                .description()
                .map(|d| d.to_string())
                .unwrap_or(String::from("")),
        );

        // Add all referencing types for this type
        for ref_type in references {
            doc.add_text(referencing_types_field, ref_type);
        }

        // TODO: index documentation descriptions

        // TODO: index field parameters so we get input types
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
    info!("Indexed {} types in {:.2?}", type_references.len(), elapsed);

    Ok(index)
}

impl Search {
    pub fn new(
        schema: Arc<Mutex<Valid<Schema>>>,
        allow_mutations: bool,
    ) -> Result<Self, IndexingError> {
        Ok(Self {
            schema: schema.clone(),
            index: index(schema, allow_mutations)?,
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

        let raw_type_name_field =
            self.index
                .schema()
                .get_field(RAW_TYPE_NAME_FIELD)
                .map_err(|e| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to get raw_type_name field: {}", e),
                        None,
                    )
                })?;
        let type_name_field = self
            .index
            .schema()
            .get_field(TYPE_NAME_FIELD)
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to get type_name field: {}", e),
                    None,
                )
            })?;
        let description_field = self
            .index
            .schema()
            .get_field(DESCRIPTION_FIELD)
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to get description field: {}", e),
                    None,
                )
            })?;

        let referencing_types_field = self
            .index
            .schema()
            .get_field(REFERENCING_TYPES_FIELD)
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to get referencing_types field: {}", e),
                    None,
                )
            })?;
        let fields_field = self.index.schema().get_field(FIELDS_FIELD).map_err(|e| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to get fields field: {}", e),
                None,
            )
        })?;

        let mut root_paths: HashSet<ScoredPath> = Default::default();

        // Search results are returned ordered by score. Use an IndexMap to preserve this ordering.
        let mut scores: IndexMap<String, f32> = Default::default();

        // TODO: sanitize terms - LLM might try to put spaces, for example

        // let mut query = BooleanQuery::new(
        //     input
        //         .terms
        //         .iter()
        //         .flat_map(|term| {
        //             let lower = &term.to_lowercase();
        //             vec![
        //                 Term::from_field_text(type_name_field, lower),
        //                 Term::from_field_text(description_field, lower),
        //                 Term::from_field_text(fields_field, lower),
        //                 // TODO: index referencing types?
        //             ]
        //         })
        //         .map(|term| {
        //             (
        //                 Occur::Should,
        //                 Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
        //             )
        //         })
        //         .collect(),
        // );
        // query.set_minimum_number_should_match(1);

        // TODO: remove this once we can replicate it above
        let query = tantivy::query::QueryParser::for_index(
                &self.index,
                vec![
                    type_name_field,
                    description_field,
                    fields_field,
                ],
            )
            .parse_query(&input.terms.iter().join(" OR "))
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("Invalid search terms: {}", e),
                    None,
                )
            })?;

        println!("Query: {:?}", query);

        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(100))
            .map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to search: {}", e),
                    None,
                )
            })?;

        println!("Found {} matching documents", top_docs.len());

        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address).map_err(|e| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to get document: {}", e),
                    None,
                )
            })?;

            let type_name = doc
                .get_first(raw_type_name_field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        "Failed to extract type name from document".to_string(),
                        None,
                    )
                })?;

            scores.insert(type_name.to_string(), score);
        }

        for (type_name, score) in scores.iter().take(10) {
            // TODO: combine the score with the score for each type on the path to get a total
            //  ranking for the root path
            let mut root_path_score = *score;

            // Build up root paths by looking up referencing types
            let mut visited = HashSet::new();
            let mut queue: VecDeque<Path<'static>> = VecDeque::new();
            let mut term_root_paths = vec![];

            // Start with the current type as a Path
            let start_type = NamedType::new_unchecked(type_name);
            let start_path = Path::new(vec![&start_type]);
            // Convert Path<'_> to Path<'static> by cloning the NamedType into an owned String
            let owned_types: Vec<Cow<'static, NamedType>> = start_path
                .types
                .iter()
                .map(|t| Cow::Owned(NamedType::new_unchecked(t.to_string().as_str())))
                .collect();
            let start_path_static = Path { types: owned_types };
            queue.push_back(start_path_static);

            while let Some(current_path) = queue.pop_front() {
                if term_root_paths.len() >= MAX_ROOT_PATHS {
                    break; // Found maximum number of root types
                }

                let current_type = match current_path.types.last() {
                    Some(t) => t.to_string(),
                    None => {
                        return Err(McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            "Path has no types; cannot determine current type".to_string(),
                            None,
                        ));
                    }
                };
                if visited.contains(&current_type) {
                    continue; // Avoid cycles
                }
                visited.insert(current_type.clone());

                // Create a query to find the document for the current type
                let term = Term::from_field_text(raw_type_name_field, current_type.as_str());
                let type_query = TermQuery::new(term, IndexRecordOption::Basic);
                let type_search = searcher
                    .search(&type_query, &TopDocs::with_limit(1))
                    .map_err(|e| {
                        McpError::new(
                            ErrorCode::INTERNAL_ERROR,
                            format!("Failed to search for type document: {}", e),
                            None,
                        )
                    })?;
                let current_type_doc: Option<TantivyDocument> =
                    type_search.first().and_then(|(_, type_doc_address)| {
                        searcher
                            .doc(*type_doc_address)
                            .map_err(|e| {
                                McpError::new(
                                    ErrorCode::INTERNAL_ERROR,
                                    format!("Failed to get type document: {}", e),
                                    None,
                                )
                            })
                            .ok()
                    });

                let referencing_types: Vec<String> = if let Some(type_doc) = current_type_doc {
                    type_doc
                        .get_all(referencing_types_field)
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                } else {
                    // println!("TYPE NOT FOUND: {current_type}"); // TODO
                    Vec::new()
                };

                // The score of each type in the root path contributes to the total score of the path
                // TODO: tweak this a bit - longer paths are still getting ranked higher in some cases, and short paths have very small values
                if let Some(score) = scores.get(&current_type) {
                    root_path_score += *score;
                }

                // Each type in the path reduces the score so shorter paths are ranked higher
                // TODO: very deep type hierarchies could underflow
                root_path_score *= 0.8f32.powf((current_path.types.len() - 1) as f32);

                if referencing_types.is_empty() {
                    // This is a root type (no referencing types)
                    let mut root_path = current_path.clone();
                    root_path.types.reverse();
                    root_paths.insert(ScoredPath::new(root_path.clone(), root_path_score));
                    term_root_paths.push(root_path);
                } else {
                    // Continue traversing up to a root type
                    for ref_type in referencing_types {
                        if !visited.contains(&ref_type) {
                            let mut new_types = current_path.types.clone();
                            new_types.push(Cow::Owned(NamedType::new_unchecked(&ref_type)));
                            let new_path = Path { types: new_types };
                            queue.push_back(new_path);
                        }
                    }
                }
            }

            // TODO: add fields to the path so we can include just the relevant fields for non-leaf types?
        }

        // Take the top paths by score
        // TODO: cap total size
        let mut root_paths = root_paths.iter().collect::<Vec<_>>();
        root_paths.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        root_paths.truncate(5);

        println!(
            "\n\n\nRoot paths for search terms: {}",
            input.terms.join(", ")
        );
        for root_path in root_paths.iter() {
            println!("{root_path}");
        }

        for root_path in root_paths {
            let types = root_path.path.types.clone();
            let path_len = types.len();
            for (i, type_name) in types.into_iter().enumerate() {
                if let Some(extended_type) = schema.types.get(type_name.as_ref()) {
                    let depth = if i == path_len - 1 {
                        DepthLimit::Limited(1) // TODO - add more information about leaf type children?
                    } else {
                        DepthLimit::Limited(1)
                    };
                    tree_shaker.retain_type(extended_type, depth)
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

    #[rstest]
    #[tokio::test]
    async fn test_referencing_types_are_collected(schema: Valid<Schema>) {
        let schema = Arc::new(Mutex::new(schema));
        let search = Search::new(schema.clone(), true).expect("Failed to create search tool");

        // Search for a type that should have references
        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
            })
            .await
            .expect("Search execution failed");

        assert!(!result.is_error.unwrap_or(false));

        // The search should return the User type and potentially other related types
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

        // Verify that the User type is found
        assert!(
            content.contains("User"),
            "Expected to find User type in search results"
        );
    }
}
