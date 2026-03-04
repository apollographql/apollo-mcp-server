//! Library for indexing and searching GraphQL schemas.
//!
//! To build the index, the types in the schema are traversed depth-first, starting with a set of
//! supplied root types (Query, Mutation, Subscription). Each type encountered in the traversal is
//! indexed by:
//!
//! * The type name
//! * The type description
//! * The field names
//!
//! Searching for a set of terms returns the top root paths to types matching the search terms.
//! A root path is a path from a root type (Query, Mutation, or Subscription) to the type. This
//! provides not only information about the type itself, but also how to construct a query to
//! retrieve that type.
//!
//! Shorter paths are preferred by a customizable boost factor. If parent types in the path also
//! match the search terms, a customizable portion of their scores are added to the path score.
//! The total number of matching types considered can be customized, as can the maximum number of
//! paths to each type (types may be reachable by more than one path - the shortest paths to root
//! take precedence over longer paths).

use crate::path::PathNode;
use apollo_compiler::ast::{NamedType, OperationType as AstOperationType};
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use enumset::{EnumSet, EnumSetType};
use error::{IndexingError, SearchError};
use heck::ToSnakeCase;
use itertools::Itertools;
use path::Scored;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{
    Index, TantivyDocument, Term,
    schema::{STORED, Schema as TantivySchema},
};
use tracing::{Level, debug, error, info, warn};
use traverse::SchemaExt;

pub mod error;
mod path;
mod traverse;

pub const TYPE_NAME_FIELD: &str = "type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const FIELDS_FIELD: &str = "fields";
pub const RAW_TYPE_NAME_FIELD: &str = "raw_type_name";
pub const REFERENCING_TYPES_FIELD: &str = "referencing_types";

/// Types of operations to be included in the schema index. Unlike the AST types, these types can
/// be included in an [`EnumSet`].
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

pub struct Options {
    /// The maximum number of matching schema types to include in the results
    pub max_type_matches: usize,

    /// The maximum number of paths to root to include for each matching schema type
    pub max_paths_per_type: usize,

    /// The boost factor applied to shorter paths to root (0.0 for no boost, 1.0 for 100% boost)
    pub short_path_boost_factor: f32,

    /// The percentage of the score of each parent type added to the overall score of the path
    /// to root 0.0 for 0%, 1.0 for 100%)
    pub parent_match_boost_factor: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_type_matches: 10,
            max_paths_per_type: 3,
            short_path_boost_factor: 0.5,
            parent_match_boost_factor: 0.2,
        }
    }
}

/// Splits camelCase and PascalCase identifiers in the given text into space-separated words.
///
/// Each word-like segment (contiguous alphanumeric characters) is converted from camelCase to
/// snake_case using `heck`, then underscores are replaced with spaces. Non-alphanumeric
/// characters are preserved as-is so that Tantivy's `SimpleTokenizer` can still split on them.
///
/// Examples:
/// - `"CreatePostInput"` → `"create post input"`
/// - `"fieldName: TypeName"` → `"field name: type name"`
fn expand_identifiers(text: &str) -> String {
    let mut result = String::with_capacity(text.len() * 2);
    let mut word_start = None;

    for (i, ch) in text.char_indices() {
        if ch.is_alphanumeric() || ch == '_' {
            if word_start.is_none() {
                word_start = Some(i);
            }
        } else {
            if let Some(start) = word_start {
                push_expanded_word(&mut result, &text[start..i]);
                word_start = None;
            }
            result.push(ch);
        }
    }

    if let Some(start) = word_start {
        push_expanded_word(&mut result, &text[start..]);
    }

    result
}

/// Converts a single camelCase word to space-separated lowercase words and appends to `out`.
/// Consecutive underscores are collapsed to a single space, matching Rover's
/// `.filter(|w| !w.is_empty())` behavior.
fn push_expanded_word(out: &mut String, word: &str) {
    let mut prev_underscore = false;
    for ch in word.to_snake_case().chars() {
        if ch == '_' {
            if !prev_underscore {
                out.push(' ');
            }
            prev_underscore = true;
        } else {
            out.push(ch);
            prev_underscore = false;
        }
    }
}

#[derive(Clone)]
pub struct SchemaIndex {
    inner: Index,
    text_analyzer: TextAnalyzer,
    raw_type_name_field: Field,
    type_name_field: Field,
    description_field: Field,
    fields_field: Field,
    referencing_types_field: Field,
}

impl SchemaIndex {
    #[tracing::instrument(skip_all, name = "schema_index")]
    pub fn new(
        schema: &Valid<Schema>,
        root_types: EnumSet<OperationType>,
        index_memory_bytes: usize,
    ) -> Result<Self, IndexingError> {
        let start_time = Instant::now();

        // Register a custom analyzer with English stemming and lowercasing
        // TODO: support other languages
        let text_analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
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

        // The raw type name is indexed as the exact name (no stemming or lowercasing)
        let raw_type_name_field = index_schema.add_text_field(
            RAW_TYPE_NAME_FIELD,
            TextOptions::default()
                .set_indexing_options(TextFieldIndexing::default().set_tokenizer("raw"))
                .set_stored(),
        );
        let referencing_types_field = index_schema.add_text_field(REFERENCING_TYPES_FIELD, STORED);

        // Create the index
        let index_schema = index_schema.build();
        let index = Index::create_in_ram(index_schema);
        index
            .tokenizers()
            .register("en_stem", text_analyzer.clone());

        // Map every type in the schema to the types referencing it
        let mut index_writer = index.writer(index_memory_bytes)?;
        let mut type_references: HashMap<String, Vec<String>> = HashMap::default();
        for (extended_type, path) in schema.traverse(root_types) {
            let entry = type_references
                .entry(extended_type.name().to_string())
                .or_default();
            if let Some((ref_type, field_name, field_args)) = path.referencing_type() {
                if let Some(field_name) = field_name {
                    entry.push(format!(
                        "{}#{}{}",
                        ref_type,
                        field_name.as_str(),
                        if field_args.is_empty() {
                            "".to_string()
                        } else {
                            format!("#{}", field_args.iter().join(","))
                        }
                    ));
                } else {
                    entry.push(ref_type.to_string())
                }
            }
        }

        if tracing::enabled!(Level::DEBUG) {
            for (type_name, references) in &type_references {
                debug!("Type '{}' is referenced by: {:?}", type_name, references);
            }
        }

        // Build an index of each type
        for (type_name, references) in &type_references {
            let type_name = NamedType::new_unchecked(type_name.as_str());
            let extended_type = if let Some(extended_type) = schema.types.get(&type_name) {
                extended_type
            } else {
                // This can never really happen since we got the type name from the schema above
                continue;
            };
            if extended_type.is_built_in() {
                continue;
            }

            // Create a document for each type
            let mut doc = TantivyDocument::default();
            doc.add_text(type_name_field, expand_identifiers(extended_type.name()));
            doc.add_text(raw_type_name_field, extended_type.name());
            doc.add_text(
                description_field,
                extended_type
                    .description()
                    .map(|d| expand_identifiers(d))
                    .unwrap_or_default(),
            );

            for ref_type in references {
                doc.add_text(referencing_types_field, ref_type);
            }
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
            doc.add_text(fields_field, expand_identifiers(&fields));
            let field_descriptions = match extended_type {
                ExtendedType::Enum(enum_type) => enum_type
                    .values
                    .iter()
                    .flat_map(|(_, value)| value.description.as_ref())
                    .map(|node| node.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                ExtendedType::Object(obj) => obj
                    .fields
                    .iter()
                    .flat_map(|(_, field)| field.description.as_ref())
                    .map(|node| node.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                ExtendedType::Interface(interface) => interface
                    .fields
                    .iter()
                    .flat_map(|(_, field)| field.description.as_ref())
                    .map(|node| node.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                ExtendedType::InputObject(input) => input
                    .fields
                    .iter()
                    .flat_map(|(_, field)| field.description.as_ref())
                    .map(|node| node.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            doc.add_text(description_field, expand_identifiers(&field_descriptions));
            index_writer.add_document(doc)?;
        }
        index_writer.commit()?;

        let elapsed = start_time.elapsed();
        info!("Indexed {} types in {:.2?}", type_references.len(), elapsed);

        Ok(Self {
            inner: index,
            text_analyzer,
            raw_type_name_field,
            type_name_field,
            description_field,
            fields_field,
            referencing_types_field,
        })
    }

    /// Search the schema for a set of terms
    pub fn search<I>(
        &self,
        terms: I,
        options: Options,
    ) -> Result<Vec<Scored<PathNode>>, SearchError>
    where
        I: IntoIterator<Item = String>,
    {
        let searcher = self.inner.reader()?.searcher();
        let mut root_paths: Vec<Scored<PathNode>> = Default::default();
        let mut scores: IndexMap<String, f32> = Default::default();

        let query = self.query(terms);
        debug!("Index query: {:?}", query);

        // Get the top GraphQL schema types matching the search terms
        let top_docs = searcher.search(&query, &TopDocs::with_limit(100))?;

        // Map each type name to its score
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            if let Some(type_name) = doc
                .get_first(self.raw_type_name_field)
                .and_then(|v| v.as_str())
            {
                debug!(
                    "Explanation for {type_name}: {:?}",
                    query.explain(&searcher, doc_address)?
                );
                scores.insert(type_name.to_string(), score);
            } else {
                // This should never happen, since every document we add has this field defined
                error!("Doc address {doc_address:?} missing raw type name field");
            }
        }

        // For the top M types, compute the top N root paths to that type
        for (type_name, score) in scores.iter().take(options.max_type_matches) {
            let mut root_path_score = *score;

            // Build up root paths by looking up referencing types
            let mut visited = HashSet::new();
            let mut queue = VecDeque::new();
            let mut root_path_count = 0usize;

            // Start with the current type as a Path
            queue.push_back(PathNode::new(NamedType::new_unchecked(type_name)));

            while let Some(current_path) = queue.pop_front() {
                if root_path_count >= options.max_paths_per_type {
                    break;
                }
                let current_type = current_path.node_type.to_string();
                visited.insert(current_type.clone());

                // Create a query to find the document for the current type
                let term = Term::from_field_text(self.raw_type_name_field, current_type.as_str());
                let type_query = TermQuery::new(term, IndexRecordOption::Basic);
                let type_search = searcher.search(&type_query, &TopDocs::with_limit(1))?;
                let current_type_doc: Option<TantivyDocument> = type_search
                    .first()
                    .and_then(|(_, type_doc_address)| searcher.doc(*type_doc_address).ok());
                let referencing_types: Vec<String> = if let Some(type_doc) = current_type_doc {
                    type_doc
                        .get_all(self.referencing_types_field)
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                } else {
                    // This should never happen since the type was found in the schema traversal
                    warn!(type_name = current_type, "Type not found");
                    Vec::new()
                };

                // The score of each type in the root path contributes to the total score of the path
                if let Some(score) = scores.get(&current_type) {
                    root_path_score += options.parent_match_boost_factor * *score;
                }

                if referencing_types.is_empty() {
                    // This is a root type (no referencing types)
                    let root_path = current_path.clone();
                    root_paths.push(Scored::new(root_path, root_path_score));
                    root_path_count += 1;
                } else {
                    // Continue traversing up to a root type
                    for ref_type in referencing_types {
                        let (type_name, field_name, field_args) =
                            if let Some((type_name, field_name)) = ref_type.split_once('#') {
                                if let Some((field_name, field_args)) = field_name.split_once('#') {
                                    (
                                        type_name.to_string(),
                                        Some(Name::new_unchecked(field_name)),
                                        field_args
                                            .split(',')
                                            .map(|arg| Name::new_unchecked(arg.trim()))
                                            .collect::<Vec<_>>(),
                                    )
                                } else {
                                    (
                                        type_name.to_string(),
                                        Some(Name::new_unchecked(field_name)),
                                        vec![],
                                    )
                                }
                            } else {
                                (ref_type.clone(), None, vec![])
                            };
                        if !visited.contains(&ref_type) {
                            queue.push_back(current_path.clone().add_parent(
                                field_name,
                                field_args,
                                NamedType::new_unchecked(&type_name),
                            ));
                        }
                    }
                }
            }
        }

        Ok(self
            .boost_shorter_paths(root_paths, options.short_path_boost_factor)
            .into_iter()
            .sorted_by(|a, b| {
                b.score()
                    .partial_cmp(&a.score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .collect::<Vec<_>>())
    }

    /// Apply a boost factor to shorter paths
    fn boost_shorter_paths(
        &self,
        scored_paths: Vec<Scored<PathNode>>,
        boost_factor: f32,
    ) -> Vec<Scored<PathNode>> {
        if scored_paths.is_empty() || boost_factor == 0f32 {
            return scored_paths;
        }

        // Calculate the range of path lengths
        let path_lengths: Vec<usize> = scored_paths
            .iter()
            .map(|scored| scored.inner.len())
            .collect();
        let min_length = *path_lengths.iter().min().unwrap_or(&1);
        let max_length = *path_lengths.iter().max().unwrap_or(&1);

        // Only apply boost if there's a range in path lengths
        if max_length <= min_length {
            return scored_paths;
        }

        let length_range = (max_length - min_length) as f32;

        // Apply normalized boost to each path
        scored_paths
            .into_iter()
            .map(|scored_path| {
                let path_length = scored_path.inner.len();
                let normalized_length = (path_length - min_length) as f32 / length_range;
                // Boost shorter paths: 1.0 for shortest, 0.0 for longest
                let length_boost = 1.0 - normalized_length;
                let boosted_score = scored_path.score() * (1.0 + boost_factor * length_boost);
                Scored::new(scored_path.inner, boosted_score)
            })
            .collect()
    }

    /// Create the query used to search for a given set of terms.
    fn query<I>(&self, terms: I) -> impl Query
    where
        I: IntoIterator<Item = String>,
    {
        let mut text_analyzer = self.text_analyzer.clone();
        let mut query = BooleanQuery::new(
            terms
                .into_iter()
                .flat_map(|term| {
                    let expanded = expand_identifiers(&term);
                    let mut terms: Vec<Term> = Vec::new();
                    let mut token_stream = text_analyzer.token_stream(&expanded);
                    token_stream.process(&mut |token| {
                        terms.push(Term::from_field_text(self.type_name_field, &token.text));
                        terms.push(Term::from_field_text(self.description_field, &token.text));
                        terms.push(Term::from_field_text(self.fields_field, &token.text));
                    });
                    terms
                })
                .map(|term| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                    )
                })
                .collect(),
        );
        query.set_minimum_number_should_match(1);
        query
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use rstest::{fixture, rstest};

    const TEST_SCHEMA: &str = include_str!("testdata/schema.graphql");

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    #[rstest]
    fn search(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            15_000_000,
        )
        .unwrap();

        let results = search
            .search(vec!["dimensions".to_string()], Options::default())
            .unwrap();

        assert_snapshot!(
            results
                .iter()
                .take(10)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[rstest]
    fn search_interface_implementer_fields(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            15_000_000,
        )
        .unwrap();

        let results = search
            .search(vec!["username".to_string()], Options::default())
            .unwrap();

        assert!(
            !results.is_empty(),
            "Should find results for 'username' field"
        );

        let paths: Vec<String> = results.iter().map(ToString::to_string).collect();
        let found_user = paths.iter().any(|p| p.contains("User"));

        assert!(
            found_user,
            "Should find User type when searching for username field (User implements Node).\nFound paths:\n{}",
            paths.join("\n")
        );

        let results = search
            .search(vec!["analytics".to_string()], Options::default())
            .unwrap();

        assert!(
            !results.is_empty(),
            "Should find results for 'analytics' field"
        );

        let paths: Vec<String> = results.iter().map(ToString::to_string).collect();
        let found_post = paths.iter().any(|p| p.contains("Post"));

        assert!(
            found_post,
            "Should find Post type when searching for 'analytics' field (which only exists on Post, not on Node/Content interfaces).\nFound paths:\n{}",
            paths.join("\n")
        );
    }

    #[rstest]
    fn search_camel_case_splitting(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            15_000_000,
        )
        .unwrap();

        // Searching "post" should match camelCase identifiers like PostAnalytics and UpdatePostInput
        // via word-boundary splitting (e.g. PostAnalytics -> "post analytics")
        let results = search
            .search(vec!["post".to_string()], Options::default())
            .unwrap();

        let paths: Vec<String> = results.iter().map(ToString::to_string).collect();
        let has_post_analytics = paths.iter().any(|p| p.contains("PostAnalytics"));
        let has_update_post_input = paths.iter().any(|p| p.contains("UpdatePostInput"));

        assert!(
            has_post_analytics,
            "Should find PostAnalytics when searching for 'post' (camelCase split).\nFound paths:\n{}",
            paths.join("\n")
        );
        assert!(
            has_update_post_input,
            "Should find UpdatePostInput when searching for 'post' (camelCase split).\nFound paths:\n{}",
            paths.join("\n")
        );
    }

    #[rstest]
    fn search_camel_case_query_term(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            15_000_000,
        )
        .unwrap();

        // Searching "CreatePost" should also work via camelCase splitting of the query term
        let results = search
            .search(vec!["CreatePost".to_string()], Options::default())
            .unwrap();

        let paths: Vec<String> = results.iter().map(ToString::to_string).collect();
        let has_post = paths.iter().any(|p| p.contains("Post"));

        assert!(
            has_post,
            "Should find Post-related types when searching for 'CreatePost' (query term camelCase split).\nFound paths:\n{}",
            paths.join("\n")
        );
    }

    #[rstest]
    fn search_camel_case_in_description(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            15_000_000,
        )
        .unwrap();

        // Tag's description contains "createPost", so searching "post" should match via
        // camelCase splitting of the description at index time.
        let results = search
            .search(vec!["post".to_string()], Options::default())
            .unwrap();

        let paths: Vec<String> = results.iter().map(ToString::to_string).collect();
        assert!(
            paths.iter().any(|p| p.contains("Tag")),
            "Should find Tag when searching for 'post' (camelCase in description).\nFound paths:\n{}",
            paths.join("\n")
        );
    }

    #[rstest]
    #[case::pascal_case("CreatePostInput", "create post input")]
    #[case::camel_case("createPost", "create post")]
    #[case::camel_case_multi("getUserById", "get user by id")]
    #[case::pascal_compound("PostConnection", "post connection")]
    #[case::uppercase_run("HTMLParser", "html parser")]
    #[case::single_word("post", "post")]
    #[case::acronym("ID", "id")]
    #[case::snake_case_input("get_user_by_id", "get user by id")]
    #[case::with_colon_separator("fieldName: TypeName", "field name: type name")]
    #[case::with_comma_separator("firstName, lastName", "first name, last name")]
    fn expand_identifiers_splits_at_word_boundaries(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(expand_identifiers(input), expected);
    }
}
