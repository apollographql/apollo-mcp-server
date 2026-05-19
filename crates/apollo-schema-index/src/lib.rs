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
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use enumset::{EnumSet, EnumSetType};
use error::{IndexingError, SearchError};
use heck::ToSnakeCase;
use itertools::Itertools;
use path::Scored;
use std::collections::HashSet;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{
    Language, LowerCaser, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer,
};
use tantivy::{
    Index, TantivyDocument, Term,
    schema::{STORED, Schema as TantivySchema},
};
use tracing::{debug, error, info};
use traverse::SchemaExt;

pub mod error;
mod path;
mod traverse;

/// English stop words filtered from the analyzer pipeline. Matches the list used
/// by Tantivy's bundled `StopWordFilter::new(Language::English)`, which is itself
/// the Apache Lucene `EnglishAnalyzer` set. Filtering these at index AND query
/// time prevents low-information tokens like "by" from polluting matches (e.g.
/// "day by day" no longer matches a search for "userByEmail").
const ENGLISH_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

pub const TYPE_NAME_FIELD: &str = "type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const FIELDS_FIELD: &str = "fields";
pub const FIELD_NAMES_FIELD: &str = "field_names";
pub const RAW_TYPE_NAME_FIELD: &str = "raw_type_name";
/// Discriminator: "type" for per-type docs, "field" for per-root-field docs.
pub const DOC_KIND_FIELD: &str = "doc_kind";
/// Stored metadata for field docs: parent operation type name (Query/Mutation/Subscription).
pub const FIELD_PARENT_TYPE_FIELD: &str = "field_parent_type";
/// Stored metadata for field docs: the field name itself, exact form.
pub const FIELD_NAME_FIELD: &str = "field_name";
/// Stored metadata for field docs: comma-joined argument *type* names.
pub const FIELD_ARG_TYPES_FIELD: &str = "field_arg_types";
/// Stored metadata for field docs: the return type (inner named type).
pub const FIELD_RETURN_TYPE_FIELD: &str = "field_return_type";

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

/// Search options. Currently empty — kept as a struct for API stability so future
/// tuning knobs (e.g. score-ratio cutoff override) can be added without breaking
/// callers. Previously contained path-scoring knobs that are no longer needed
/// after the BFS path-builder was removed.
#[derive(Default)]
pub struct Options;

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
    field_names_field: Field,
    doc_kind_field: Field,
    field_parent_type_field: Field,
    field_name_field: Field,
    field_arg_types_field: Field,
    field_return_type_field: Field,
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
            .filter(StopWordFilter::remove(
                ENGLISH_STOPWORDS.iter().map(|w| (*w).to_string()),
            ))
            .filter(Stemmer::new(Language::English))
            .build();

        // Create the schema builder and add fields with the custom analyzer
        let mut index_schema = TantivySchema::builder();
        let type_name_field = index_schema.add_text_field(
            TYPE_NAME_FIELD,
            TextOptions::default()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("en_stem")
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                )
                .set_stored(),
        );
        let description_field = index_schema.add_text_field(
            DESCRIPTION_FIELD,
            TextOptions::default()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("en_stem")
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                )
                .set_stored(),
        );
        let fields_field = index_schema.add_text_field(
            FIELDS_FIELD,
            TextOptions::default()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("en_stem")
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                )
                .set_stored(),
        );
        // Field names only (without return types), so BM25 length normalization
        // doesn't bury matches inside the longer `fields` blob.
        let field_names_field = index_schema.add_text_field(
            FIELD_NAMES_FIELD,
            TextOptions::default()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("en_stem")
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                )
                .set_stored(),
        );

        // The raw type name is indexed as the exact name (no stemming or lowercasing)
        let raw_type_name_field = index_schema.add_text_field(
            RAW_TYPE_NAME_FIELD,
            TextOptions::default()
                .set_indexing_options(TextFieldIndexing::default().set_tokenizer("raw"))
                .set_stored(),
        );
        // Stored-only metadata fields for per-root-field docs. None of these are
        // indexed for search — they're consulted only after a doc match to
        // discriminate type-docs vs field-docs and to reconstruct the field path.
        let doc_kind_field = index_schema.add_text_field(DOC_KIND_FIELD, STORED);
        let field_parent_type_field =
            index_schema.add_text_field(FIELD_PARENT_TYPE_FIELD, STORED);
        let field_name_field = index_schema.add_text_field(FIELD_NAME_FIELD, STORED);
        let field_arg_types_field = index_schema.add_text_field(FIELD_ARG_TYPES_FIELD, STORED);
        let field_return_type_field =
            index_schema.add_text_field(FIELD_RETURN_TYPE_FIELD, STORED);

        // Create the index
        let index_schema = index_schema.build();
        let index = Index::create_in_ram(index_schema);
        index
            .tokenizers()
            .register("en_stem", text_analyzer.clone());

        // Walk the schema from the supplied root operation types and collect the
        // set of reachable type names. Each will get its own type doc below.
        let mut index_writer = index.writer(index_memory_bytes)?;
        let mut reachable_types: HashSet<String> = HashSet::new();
        for (extended_type, _path) in schema.traverse(root_types) {
            reachable_types.insert(extended_type.name().to_string());
        }

        // Build an index of each reachable type
        for type_name in &reachable_types {
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
            let field_names = match extended_type {
                ExtendedType::Object(obj) => obj.fields.keys().join(" "),
                ExtendedType::Interface(interface) => interface.fields.keys().join(" "),
                ExtendedType::InputObject(input) => input.fields.keys().join(" "),
                ExtendedType::Enum(enum_type) => enum_type.values.keys().join(" "),
                _ => String::new(),
            };
            doc.add_text(field_names_field, expand_identifiers(&field_names));
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
            doc.add_text(doc_kind_field, "type");
            index_writer.add_document(doc)?;
        }

        // Emit one doc per root operation field (Query/Mutation/Subscription direct
        // fields only — not fields on nested types). These let agent-style searches
        // for operation names land directly on the operation, rather than going
        // through the return type's document.
        let mut field_doc_count = 0usize;
        for op_kind in [
            AstOperationType::Query,
            AstOperationType::Mutation,
            AstOperationType::Subscription,
        ] {
            if !root_types.contains(OperationType::from(op_kind)) {
                continue;
            }
            let Some(root_name) = schema.root_operation(op_kind) else {
                continue;
            };
            let Some(ExtendedType::Object(root_type)) = schema.types.get(root_name) else {
                continue;
            };

            for (field_name, field_def) in root_type.fields.iter() {
                let return_type = field_def.ty.inner_named_type().to_string();
                let arg_type_names: Vec<String> = field_def
                    .arguments
                    .iter()
                    .map(|arg| arg.ty.inner_named_type().to_string())
                    .collect();

                // Searchable text combines the field name with its arg type names so
                // searches that mention an arg type (e.g. "Email") can also land here.
                // Field name is also duplicated into type_name_field and field_names_field
                // so it gets the same scoring treatment as a short type-name match.
                let expanded_field_name = expand_identifiers(field_name.as_str());
                let args_searchable = arg_type_names
                    .iter()
                    .map(|n| expand_identifiers(n))
                    .collect::<Vec<_>>()
                    .join(" ");

                let mut doc = TantivyDocument::default();
                doc.add_text(type_name_field, &expanded_field_name);
                doc.add_text(field_names_field, &expanded_field_name);
                doc.add_text(
                    fields_field,
                    if args_searchable.is_empty() {
                        expanded_field_name.clone()
                    } else {
                        format!("{} {}", expanded_field_name, args_searchable)
                    },
                );
                doc.add_text(
                    description_field,
                    field_def
                        .description
                        .as_ref()
                        .map(|d| expand_identifiers(d))
                        .unwrap_or_default(),
                );
                // Stored metadata used by search() to reconstruct the path directly.
                doc.add_text(doc_kind_field, "field");
                doc.add_text(field_parent_type_field, root_name.as_str());
                doc.add_text(field_name_field, field_name.as_str());
                doc.add_text(field_arg_types_field, arg_type_names.join(","));
                doc.add_text(field_return_type_field, &return_type);

                index_writer.add_document(doc)?;
                field_doc_count += 1;
            }
        }
        index_writer.commit()?;

        let elapsed = start_time.elapsed();
        info!(
            "Indexed {} types and {} root operation fields in {:.2?}",
            reachable_types.len(),
            field_doc_count,
            elapsed
        );

        Ok(Self {
            inner: index,
            text_analyzer,
            raw_type_name_field,
            type_name_field,
            description_field,
            fields_field,
            field_names_field,
            doc_kind_field,
            field_parent_type_field,
            field_name_field,
            field_arg_types_field,
            field_return_type_field,
        })
    }

    /// Search the schema for a set of terms
    pub fn search<I>(
        &self,
        terms: I,
        _options: Options,
    ) -> Result<Vec<Scored<PathNode>>, SearchError>
    where
        I: IntoIterator<Item = String>,
    {
        let searcher = self.inner.reader()?.searcher();
        let mut root_paths: Vec<Scored<PathNode>> = Default::default();

        let query = self.query(terms);
        debug!("Index query: {:?}", query);

        // Get the top matching documents (mix of type-docs and field-docs).
        let top_docs = searcher.search(&query, &TopDocs::with_limit(100))?;

        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let kind = doc
                .get_first(self.doc_kind_field)
                .and_then(|v| v.as_str())
                .unwrap_or("type");

            if kind == "field" {
                // Field-doc: reconstruct the two-node path
                // (parent -> field(args) -> return_type) from stored metadata.
                let parent = doc
                    .get_first(self.field_parent_type_field)
                    .and_then(|v| v.as_str());
                let field_name = doc
                    .get_first(self.field_name_field)
                    .and_then(|v| v.as_str());
                let return_type = doc
                    .get_first(self.field_return_type_field)
                    .and_then(|v| v.as_str());
                let arg_types_raw = doc
                    .get_first(self.field_arg_types_field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match (parent, field_name, return_type) {
                    (Some(parent), Some(field), Some(ret)) => {
                        let field_args: Vec<NamedType> = arg_types_raw
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .map(Name::new_unchecked)
                            .collect();

                        debug!(
                            "Explanation for field {}.{}: {:?}",
                            parent,
                            field,
                            query.explain(&searcher, doc_address)?
                        );

                        let path = PathNode::new(NamedType::new_unchecked(parent)).add_child(
                            Some(Name::new_unchecked(field)),
                            field_args,
                            NamedType::new_unchecked(ret),
                        );
                        root_paths.push(Scored::new(path, score));
                    }
                    _ => {
                        error!(
                            "Field doc at {doc_address:?} missing required metadata: \
                             parent={parent:?} field={field_name:?} return={return_type:?}"
                        );
                    }
                }
                continue;
            }

            // Type-doc: emit a single-node path. Tree-shaking will retain just
            // this type at the configured leaf depth.
            if let Some(type_name) = doc
                .get_first(self.raw_type_name_field)
                .and_then(|v| v.as_str())
            {
                debug!(
                    "Explanation for {type_name}: {:?}",
                    query.explain(&searcher, doc_address)?
                );
                let path = PathNode::new(NamedType::new_unchecked(type_name));
                root_paths.push(Scored::new(path, score));
            } else {
                // This should never happen, since every type doc has this field defined
                error!("Doc address {doc_address:?} missing raw type name field");
            }
        }

        let mut sorted: Vec<_> = root_paths
            .into_iter()
            .sorted_by(|a, b| {
                b.score()
                    .partial_cmp(&a.score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .collect();

        const SCORE_RATIO_CUTOFF: f32 = 0.25;
        if let Some(top_score) = sorted.first().map(|s| s.score()) {
            let threshold = top_score * SCORE_RATIO_CUTOFF;
            sorted.retain(|s| s.score() >= threshold);
        }

        Ok(sorted)
    }


    /// Create the query used to search for a given set of terms.
    fn query<I>(&self, terms: I) -> impl Query
    where
        I: IntoIterator<Item = String>,
    {
        // Boost factor applied to phrase matches — multiplies the BM25 score of a phrase
        // hit before it's summed into the BooleanQuery. Higher = more weight on
        // consecutive-token matches relative to scattered TermQuery hits.
        const PHRASE_BOOST: f32 = 3.0;
        // Allowed gap between phrase tokens. Slop > 0 also takes a different code
        // path in tantivy's phrase scorer that avoids a debug_assert hit during
        // union-of-Should iteration with slop=0.
        const PHRASE_SLOP: u32 = 2;

        let mut text_analyzer = self.text_analyzer.clone();
        let searchable_fields = [
            self.type_name_field,
            self.description_field,
            self.fields_field,
            self.field_names_field,
        ];

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut all_tokens: Vec<String> = Vec::new();

        let push_phrase_clauses = |tokens: &[String],
                                   clauses: &mut Vec<(Occur, Box<dyn Query>)>| {
            if tokens.len() < 2 {
                return;
            }
            for field in searchable_fields {
                let phrase_terms: Vec<(usize, Term)> = tokens
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (i, Term::from_field_text(field, t)))
                    .collect();
                let phrase = PhraseQuery::new_with_offset_and_slop(phrase_terms, PHRASE_SLOP);
                let boosted = BoostQuery::new(Box::new(phrase), PHRASE_BOOST);
                clauses.push((Occur::Should, Box::new(boosted) as Box<dyn Query>));
            }
        };

        for term in terms {
            let expanded = expand_identifiers(&term);
            let mut tokens: Vec<String> = Vec::new();
            let mut token_stream = text_analyzer.token_stream(&expanded);
            token_stream.process(&mut |token| {
                tokens.push(token.text.clone());
            });

            // TermQuery clauses — one per (token, field) pair.
            for token in &tokens {
                for field in searchable_fields {
                    clauses.push((
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(field, token),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    ));
                }
            }

            // Per-term PhraseQuery clauses — fire when a single input term tokenizes
            // to 2+ tokens (e.g. "slack_userByEmail" → ["slack","user","email"]).
            push_phrase_clauses(&tokens, &mut clauses);

            all_tokens.extend(tokens);
        }

        // Combined PhraseQuery clauses across ALL input terms — fires when callers
        // pass multiple single-token terms (e.g. ["slack","user","email"]). For
        // single-input-term callers whose term already produced 2+ tokens, this
        // duplicates the per-term clauses; the duplication just doubles the
        // effective boost on that phrase, which is benign.
        push_phrase_clauses(&all_tokens, &mut clauses);

        let mut query = BooleanQuery::new(clauses);
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
