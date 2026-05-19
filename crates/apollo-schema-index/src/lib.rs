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
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
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

pub const PARENT_TYPE_NAME_FIELD: &str = "parent_type_name";
pub const FIELD_NAME_FIELD: &str = "field_name";
pub const ARG_NAMES_FIELD: &str = "arg_names";
pub const RETURN_TYPE_NAME_FIELD: &str = "return_type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const PARENT_TYPE_NAME_RAW_FIELD: &str = "parent_type_name_raw";
pub const FIELD_NAME_RAW_FIELD: &str = "field_name_raw";
pub const RETURN_TYPE_NAME_RAW_FIELD: &str = "return_type_name_raw";
pub const FIELD_ARGS_RAW_FIELD: &str = "field_args_raw";

/// An edge in the type-reference graph: "type X is referenced by parent_type's field_name
/// (with these field_args)".
#[derive(Clone, Debug)]
struct ReferencingEdge {
    parent_type: String,
    /// `None` when the type is reached without a field name (e.g., union member, interface
    /// implementer).
    field_name: Option<Name>,
    field_args: Vec<NamedType>,
}

/// Tantivy field handles bundled together for ergonomic doc writing.
struct DocFields {
    parent_type_name: Field,
    field_name: Field,
    arg_names: Field,
    return_type_name: Field,
    description: Field,
    parent_type_name_raw: Field,
    field_name_raw: Field,
    return_type_name_raw: Field,
    field_args_raw: Field,
}

/// A single search hit: one field doc and its BM25 score.
struct FieldHit {
    parent_type: String,
    field_name: String,
    arg_types: Vec<NamedType>,
    /// `None` for enum values.
    return_type: Option<String>,
    score: f32,
}

/// One indexable field on a type — produced by [`SchemaIndex::field_records`] and consumed
/// by [`SchemaIndex::write_field_doc`].
struct FieldRecord<'a> {
    parent_type: &'a str,
    field_name: &'a str,
    arg_names: Vec<&'a str>,
    arg_types: Vec<String>,
    /// `None` for enum values (which terminate a path without a return type).
    return_type: Option<&'a str>,
    parent_description: &'a str,
    field_description: &'a str,
}

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
    /// The maximum number of matching field hits to expand into root paths
    pub max_type_matches: usize,

    /// The maximum number of paths to root to include for each matching field hit
    pub max_paths_per_type: usize,

    /// The boost factor applied to shorter paths to root (0.0 for no boost, 1.0 for 100% boost)
    pub short_path_boost_factor: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_type_matches: 10,
            max_paths_per_type: 3,
            short_path_boost_factor: 0.5,
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
    parent_type_name_field: Field,
    field_name_field: Field,
    arg_names_field: Field,
    return_type_name_field: Field,
    description_field: Field,
    parent_type_name_raw_field: Field,
    field_name_raw_field: Field,
    return_type_name_raw_field: Field,
    field_args_raw_field: Field,
    /// In-memory map of type name → incoming edges, used for path-building at search time.
    type_references: HashMap<String, Vec<ReferencingEdge>>,
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

        let text_indexing = || TextFieldIndexing::default().set_tokenizer("en_stem");
        let mut index_schema = TantivySchema::builder();
        let parent_type_name_field = index_schema.add_text_field(
            PARENT_TYPE_NAME_FIELD,
            TextOptions::default().set_indexing_options(text_indexing()),
        );
        let field_name_field = index_schema.add_text_field(
            FIELD_NAME_FIELD,
            TextOptions::default().set_indexing_options(text_indexing()),
        );
        let arg_names_field = index_schema.add_text_field(
            ARG_NAMES_FIELD,
            TextOptions::default().set_indexing_options(text_indexing()),
        );
        let return_type_name_field = index_schema.add_text_field(
            RETURN_TYPE_NAME_FIELD,
            TextOptions::default().set_indexing_options(text_indexing()),
        );
        let description_field = index_schema.add_text_field(
            DESCRIPTION_FIELD,
            TextOptions::default().set_indexing_options(text_indexing()),
        );

        // Raw identifier fields preserve exact casing for lookup and display.
        let raw_indexing = || {
            TextOptions::default()
                .set_indexing_options(TextFieldIndexing::default().set_tokenizer("raw"))
                .set_stored()
        };
        let parent_type_name_raw_field =
            index_schema.add_text_field(PARENT_TYPE_NAME_RAW_FIELD, raw_indexing());
        let field_name_raw_field =
            index_schema.add_text_field(FIELD_NAME_RAW_FIELD, raw_indexing());
        let return_type_name_raw_field =
            index_schema.add_text_field(RETURN_TYPE_NAME_RAW_FIELD, raw_indexing());
        let field_args_raw_field = index_schema.add_text_field(FIELD_ARGS_RAW_FIELD, STORED);

        // Create the index
        let index_schema = index_schema.build();
        let index = Index::create_in_ram(index_schema);
        index
            .tokenizers()
            .register("en_stem", text_analyzer.clone());

        // Build the type-reference graph by traversing from root operation types.
        let mut type_references: HashMap<String, Vec<ReferencingEdge>> = HashMap::default();
        for (extended_type, path) in schema.traverse(root_types) {
            let entry = type_references
                .entry(extended_type.name().to_string())
                .or_default();
            if let Some((ref_type, field_name, field_args)) = path.referencing_type() {
                entry.push(ReferencingEdge {
                    parent_type: ref_type.to_string(),
                    field_name: field_name.cloned(),
                    field_args: field_args.into_iter().cloned().collect(),
                });
            }
        }

        if tracing::enabled!(Level::DEBUG) {
            for (type_name, references) in &type_references {
                debug!("Type '{}' is referenced by: {:?}", type_name, references);
            }
        }

        // Write a Tantivy doc per field (or enum value). Path-building at search time uses
        // `type_references` to walk from each hit's parent_type up to a root operation.
        let doc_fields = DocFields {
            parent_type_name: parent_type_name_field,
            field_name: field_name_field,
            arg_names: arg_names_field,
            return_type_name: return_type_name_field,
            description: description_field,
            parent_type_name_raw: parent_type_name_raw_field,
            field_name_raw: field_name_raw_field,
            return_type_name_raw: return_type_name_raw_field,
            field_args_raw: field_args_raw_field,
        };
        let mut index_writer = index.writer(index_memory_bytes)?;
        let mut field_count = 0usize;
        for type_name in type_references.keys() {
            let type_name = NamedType::new_unchecked(type_name.as_str());
            let Some(extended_type) = schema.types.get(&type_name) else {
                continue;
            };
            if extended_type.is_built_in() {
                continue;
            }

            for record in Self::field_records(extended_type) {
                Self::write_field_doc(&mut index_writer, &doc_fields, &record)?;
                field_count += 1;
            }
        }
        index_writer.commit()?;

        let elapsed = start_time.elapsed();
        info!(
            "Indexed {} fields across {} types in {:.2?}",
            field_count,
            type_references.len(),
            elapsed
        );

        Ok(Self {
            inner: index,
            text_analyzer,
            parent_type_name_field,
            field_name_field,
            arg_names_field,
            return_type_name_field,
            description_field,
            parent_type_name_raw_field,
            field_name_raw_field,
            return_type_name_raw_field,
            field_args_raw_field,
            type_references,
        })
    }

    /// Enumerate one record per indexable field (or enum value) on a type.
    fn field_records(extended_type: &ExtendedType) -> Vec<FieldRecord<'_>> {
        let parent_description = extended_type
            .description()
            .map(|d| d.as_str())
            .unwrap_or("");
        match extended_type {
            ExtendedType::Object(obj) => obj
                .fields
                .iter()
                .map(|(name, field)| FieldRecord {
                    parent_type: obj.name.as_str(),
                    field_name: name.as_str(),
                    arg_names: field.arguments.iter().map(|a| a.name.as_str()).collect(),
                    arg_types: field
                        .arguments
                        .iter()
                        .map(|a| a.ty.inner_named_type().to_string())
                        .collect(),
                    return_type: Some(field.ty.inner_named_type().as_str()),
                    parent_description,
                    field_description: field.description.as_ref().map(|n| n.as_str()).unwrap_or(""),
                })
                .collect(),
            ExtendedType::Interface(iface) => iface
                .fields
                .iter()
                .map(|(name, field)| FieldRecord {
                    parent_type: iface.name.as_str(),
                    field_name: name.as_str(),
                    arg_names: field.arguments.iter().map(|a| a.name.as_str()).collect(),
                    arg_types: field
                        .arguments
                        .iter()
                        .map(|a| a.ty.inner_named_type().to_string())
                        .collect(),
                    return_type: Some(field.ty.inner_named_type().as_str()),
                    parent_description,
                    field_description: field.description.as_ref().map(|n| n.as_str()).unwrap_or(""),
                })
                .collect(),
            ExtendedType::Enum(en) => en
                .values
                .iter()
                .map(|(name, value)| FieldRecord {
                    parent_type: en.name.as_str(),
                    field_name: name.as_str(),
                    arg_names: Vec::new(),
                    arg_types: Vec::new(),
                    return_type: None,
                    parent_description,
                    field_description: value.description.as_ref().map(|n| n.as_str()).unwrap_or(""),
                })
                .collect(),
            // Scalar/Union: no fields to index. Unions surface through their members.
            _ => Vec::new(),
        }
    }

    /// Write a single Tantivy document for a field record.
    fn write_field_doc(
        index_writer: &mut tantivy::IndexWriter,
        fields: &DocFields,
        record: &FieldRecord<'_>,
    ) -> Result<(), IndexingError> {
        let mut doc = TantivyDocument::default();
        doc.add_text(
            fields.parent_type_name,
            expand_identifiers(record.parent_type),
        );
        doc.add_text(fields.field_name, expand_identifiers(record.field_name));
        if !record.arg_names.is_empty() {
            doc.add_text(
                fields.arg_names,
                expand_identifiers(&record.arg_names.join(" ")),
            );
        }
        if let Some(rt) = record.return_type {
            doc.add_text(fields.return_type_name, expand_identifiers(rt));
            doc.add_text(fields.return_type_name_raw, rt);
        } else {
            // Enum values have no return type; index the empty string so the stored field
            // exists for retrieval.
            doc.add_text(fields.return_type_name_raw, "");
        }
        let description = match (record.parent_description, record.field_description) {
            ("", f) => f.to_string(),
            (p, "") => p.to_string(),
            (p, f) => format!("{}\n{}", p, f),
        };
        doc.add_text(fields.description, expand_identifiers(&description));
        doc.add_text(fields.parent_type_name_raw, record.parent_type);
        doc.add_text(fields.field_name_raw, record.field_name);
        for arg_type in &record.arg_types {
            doc.add_text(fields.field_args_raw, arg_type);
        }
        index_writer.add_document(doc)?;
        Ok(())
    }

    /// Search the schema for a set of terms.
    ///
    /// Returns root paths to fields (or enum values) matching the terms. The index is keyed
    /// on individual fields, not types, so a search for an operation name like `userByEmail`
    /// hits the field doc directly instead of having to outscore unrelated types that happen
    /// to mention the constituent tokens.
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

        let query = self.query(terms);
        debug!("Index query: {:?}", query);

        // Get the top GraphQL fields matching the search terms.
        let top_docs = searcher.search(&query, &TopDocs::with_limit(100))?;

        // Extract per-hit metadata. Order is preserved from Tantivy's score-descending sort.
        let mut hits: Vec<FieldHit> = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let parent_type = doc
                .get_first(self.parent_type_name_raw_field)
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let field_name = doc
                .get_first(self.field_name_raw_field)
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let return_type = doc
                .get_first(self.return_type_name_raw_field)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let arg_types: Vec<NamedType> = doc
                .get_all(self.field_args_raw_field)
                .filter_map(|v| v.as_str())
                .map(NamedType::new_unchecked)
                .collect();
            match (parent_type, field_name) {
                (Some(parent_type), Some(field_name)) => {
                    debug!(
                        "Explanation for {parent_type}.{field_name}: {:?}",
                        query.explain(&searcher, doc_address)?
                    );
                    hits.push(FieldHit {
                        parent_type,
                        field_name,
                        arg_types,
                        return_type,
                        score,
                    });
                }
                _ => {
                    error!("Doc address {doc_address:?} missing parent or field name");
                }
            }
        }

        // For each top hit, anchor a path at the hit field and walk up to root operation
        // types. Tantivy's BM25 score for the hit is the path's score; path-shape preferences
        // (shorter paths preferred) are applied below.
        for hit in hits.iter().take(options.max_type_matches) {
            let leaf_path = self.build_leaf_path(hit);
            for path in
                self.walk_up_to_roots(&hit.parent_type, leaf_path, options.max_paths_per_type)
            {
                root_paths.push(Scored::new(path, hit.score));
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

    /// Build the leaf path segment for a field hit. For regular fields, the segment is
    /// `parent_type --field_name--> return_type`. For enum values, the segment is
    /// `parent_type -> value_name` with no field-name arrow.
    fn build_leaf_path(&self, hit: &FieldHit) -> PathNode {
        if let Some(return_type) = &hit.return_type {
            PathNode::new(NamedType::new_unchecked(return_type)).add_parent(
                Some(Name::new_unchecked(&hit.field_name)),
                hit.arg_types.clone(),
                NamedType::new_unchecked(&hit.parent_type),
            )
        } else {
            PathNode::new(NamedType::new_unchecked(&hit.field_name)).add_parent(
                None,
                Vec::new(),
                NamedType::new_unchecked(&hit.parent_type),
            )
        }
    }

    /// BFS upward through the type-reference graph from `start_type` to root operation types,
    /// yielding at most `max_paths` complete root paths. Each yielded path has `leaf_path`
    /// as its rightmost segment.
    fn walk_up_to_roots(
        &self,
        start_type: &str,
        leaf_path: PathNode,
        max_paths: usize,
    ) -> Vec<PathNode> {
        let mut out = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, PathNode)> = VecDeque::new();
        queue.push_back((start_type.to_string(), leaf_path));

        while let Some((current_type, current_path)) = queue.pop_front() {
            if out.len() >= max_paths {
                break;
            }
            if !visited.insert(current_type.clone()) {
                continue;
            }
            let edges = self
                .type_references
                .get(&current_type)
                .cloned()
                .unwrap_or_default();
            if edges.is_empty() {
                // Reached a root operation type — emit the path.
                out.push(current_path);
            } else {
                for edge in edges {
                    if visited.contains(&edge.parent_type) {
                        continue;
                    }
                    let next_path = current_path.clone().add_parent(
                        edge.field_name.clone(),
                        edge.field_args.clone(),
                        NamedType::new_unchecked(&edge.parent_type),
                    );
                    queue.push_back((edge.parent_type.clone(), next_path));
                }
            }
        }
        out
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
        // A hit on the field name is the most direct signal that the field is what the user
        // is looking for, so field-name term matches get a per-token boost. Other fields
        // (parent type, args, return type, description) contribute at their unweighted BM25
        // score.
        const FIELD_NAME_BOOST: f32 = 3.0;

        let mut text_analyzer = self.text_analyzer.clone();
        let text_fields = [
            (self.parent_type_name_field, 1.0_f32),
            (self.field_name_field, FIELD_NAME_BOOST),
            (self.arg_names_field, 1.0),
            (self.return_type_name_field, 1.0),
            (self.description_field, 1.0),
        ];

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for term in terms {
            let expanded = expand_identifiers(&term);
            let mut token_stream = text_analyzer.token_stream(&expanded);
            token_stream.process(&mut |token| {
                for (field, boost) in text_fields {
                    let t = Term::from_field_text(field, &token.text);
                    let term_query: Box<dyn Query> =
                        Box::new(TermQuery::new(t, IndexRecordOption::Basic));
                    let clause: Box<dyn Query> = if (boost - 1.0).abs() > f32::EPSILON {
                        Box::new(BoostQuery::new(term_query, boost))
                    } else {
                        term_query
                    };
                    clauses.push((Occur::Should, clause));
                }
            });
        }

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

    /// Target field reaches `TargetUser`, which has minimal text matching the search tokens.
    /// Noise types are separate, are NOT reachable through the target, and saturate their
    /// own fields with `user`/`email`/`by` tokens — so a type-anchored index ranks them above
    /// `TargetUser` and the path containing `userByEmail` falls out of the top results.
    /// Verified to reproduce the production failure against the pre-fix `main` lib.rs.
    const NOISE_SCHEMA: &str = r#"
        type Query {
            userByEmail(email: String!): TargetUser
            activityStats: UserActivityStatsByDay
            emailSummary: EmailUsageStatsByUser
            dailyReport: DailyUserEmailReport
            userMetrics: UserMetricsByEmailGroup
            workspaceStats: WorkspaceUserEmailStats
        }

        type TargetUser { id: ID! }

        type UserActivityStatsByDay {
            totalUsersByDay: Int
            activeUsersByDay: Int
            emailsByDay: Int
            emailsByUser: Int
            emailUsageByDay: Int
        }

        type EmailUsageStatsByUser {
            emailsByUser: Int
            usersByEmail: Int
            emailUsageByUser: Int
            userEmailsByDay: Int
        }

        type DailyUserEmailReport {
            dailyUsers: Int
            dailyEmails: Int
            usersEmailedByDay: Int
            emailsUsersByDay: Int
        }

        type UserMetricsByEmailGroup {
            usersByEmail: Int
            emailsByUser: Int
            groupUsersByEmail: Int
            userEmailGroups: Int
        }

        type WorkspaceUserEmailStats {
            workspaceUsersByEmail: Int
            workspaceEmailsByUser: Int
            byUserActivity: Int
            byEmailLookup: Int
        }
    "#;

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    /// Regression test for field-anchored recall: searching for a specific operation name
    /// must surface that operation, even when many unrelated fields/types contain the
    /// constituent tokens. Mirrors the production failure on Slack's `userByEmail`.
    #[rstest]
    fn search_buries_target_under_token_noise() {
        let schema = Schema::parse(NOISE_SCHEMA, "noise.graphql")
            .expect("Failed to parse noise schema")
            .validate()
            .expect("Failed to validate noise schema");

        let index = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            15_000_000,
        )
        .unwrap();

        let results = index
            .search(vec!["userByEmail".to_string()], Options::default())
            .unwrap();
        let paths: Vec<String> = results.iter().map(ToString::to_string).collect();
        let rank = paths
            .iter()
            .position(|p| p.contains("userByEmail"))
            .map(|p| p + 1);

        assert!(
            matches!(rank, Some(r) if r <= 3),
            "Expected a path containing 'userByEmail' in the top 3 results, got rank {:?}.\nAll paths:\n{}",
            rank,
            paths.join("\n")
        );
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
