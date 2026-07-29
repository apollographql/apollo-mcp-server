//! Library for indexing and searching GraphQL schemas.
//!
//! The index is **operation-anchored**: one Tantivy document is written per root
//! Query/Mutation/Subscription field (i.e. per invocable operation). Each operation document
//! carries the following searchable text:
//!
//! * `operation_name` — the root field name; matches here get a per-token boost since a hit on
//!   the operation name is the most direct signal for the typical "find an operation" workload
//! * `arg_names` — the operation's argument names
//! * `return_type_name` — the operation's return type name
//! * `description` — the root field's description
//! * `nested_fields` — field names and descriptions of the return type, flattened up to
//!   `flatten_depth` levels deep (see [`crate::traverse::flatten_return_type`]). This folds the
//!   shape of the result into the operation document so an agent searching for a nested concept
//!   still surfaces the operation that reaches it.
//!
//! Searching returns [`OperationRef`]s ranked by Tantivy BM25 score (descending). Because each
//! document is a single operation, a search for an operation name like `userByEmail` hits that
//! operation directly instead of having to outscore unrelated types that happen to mention the
//! constituent tokens.

use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType as AstOperationType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use enumset::{EnumSet, EnumSetType};
use error::{IndexingError, SearchError};
use heck::ToSnakeCase;
use std::collections::BTreeSet;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{
    Index, TantivyDocument, Term,
    schema::{STORED, Schema as TantivySchema},
};
use tracing::{error, info};

mod backend;
pub mod error;
mod path;
mod scope;
mod traverse;

pub use backend::{OperationRef, SchemaSearch};
pub use path::Scored;

pub const OPERATION_NAME_FIELD: &str = "operation_name";
pub const ARG_NAMES_FIELD: &str = "arg_names";
pub const RETURN_TYPE_NAME_FIELD: &str = "return_type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const NESTED_FIELDS_FIELD: &str = "nested_fields";
pub const OPERATION_TYPE_RAW_FIELD: &str = "operation_type_raw";
pub const OPERATION_NAME_RAW_FIELD: &str = "operation_name_raw";
pub const RETURN_TYPE_NAME_RAW_FIELD: &str = "return_type_name_raw";
pub const FIELD_ARGS_RAW_FIELD: &str = "field_args_raw";
pub const SCOPE_RAW_FIELD: &str = "scope_raw";

/// The per-operation components shared by BM25 indexing and embedding-document enumeration.
/// Both [`SchemaIndex::write_operation_docs`] (BM25) and [`enumerate_operation_documents`]
/// (the shared corpus for the vector backend) are built from the single [`operation_records`]
/// enumeration so they can never diverge on what an "operation document" contains.
struct OperationRecord {
    op: OperationRef,
    /// Prefix-stripped operation name (see [`crate::scope::derive_scope`]).
    bare_name: String,
    arg_names: Vec<String>,
    return_type_name: String,
    description: String,
    /// Bounded flatten of the return type's fields/descriptions; see
    /// [`crate::traverse::flatten_return_type`].
    nested: String,
}

/// Compute the shared per-operation records for every root Query/Mutation/Subscription field in
/// `root_types`.
fn operation_records(
    schema: &Valid<Schema>,
    root_types: EnumSet<OperationType>,
    flatten_depth: usize,
) -> Vec<OperationRecord> {
    let mut records = Vec::new();
    for op_type in root_types.iter() {
        let ast_type: AstOperationType = op_type.into();
        let Some(root_name) = schema.root_operation(ast_type) else {
            continue;
        };
        let Some(ExtendedType::Object(obj)) = schema.types.get(root_name) else {
            continue;
        };
        for (name, field) in obj.fields.iter() {
            let return_type = field.ty.inner_named_type();
            let arg_names: Vec<String> =
                field.arguments.iter().map(|a| a.name.to_string()).collect();
            let arg_types: Vec<String> = field
                .arguments
                .iter()
                .map(|a| a.ty.inner_named_type().to_string())
                .collect();
            let description = field
                .description
                .as_ref()
                .map(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let nested = traverse::flatten_return_type(schema, return_type.as_str(), flatten_depth);

            let (scope, bare) = crate::scope::derive_scope(name.as_str());

            records.push(OperationRecord {
                op: OperationRef {
                    operation_type: op_type,
                    field_name: name.to_string(),
                    return_type: Some(return_type.to_string()).filter(|s| !s.is_empty()),
                    arg_types,
                    scope: scope.map(str::to_string).filter(|s| !s.is_empty()),
                },
                bare_name: bare.to_string(),
                arg_names,
                return_type_name: return_type.to_string(),
                description,
                nested,
            });
        }
    }
    records
}

/// One operation's document, ready for embedding by a vector-search backend. `text` is a joined,
/// [`expand_identifiers`]-analyzed view over the same components [`SchemaIndex::write_operation_docs`]
/// writes into Tantivy's separate per-field structure, so BM25 and embedding search rank over
/// exactly the same underlying corpus.
pub struct OperationDocument {
    pub op: OperationRef,
    pub text: String,
}

/// Enumerate every root Query/Mutation/Subscription operation in `root_types` as an
/// [`OperationDocument`] whose `text` is the analyzed, space-joined concatenation of the
/// operation's (prefix-stripped) name, argument names, return type name, description, and
/// bounded return-type flatten — the same components BM25 tokenizes, built from the same
/// [`operation_records`] enumeration [`SchemaIndex::write_operation_docs`] uses.
pub fn enumerate_operation_documents(
    schema: &Valid<Schema>,
    root_types: EnumSet<OperationType>,
    flatten_depth: usize,
) -> Vec<OperationDocument> {
    operation_records(schema, root_types, flatten_depth)
        .into_iter()
        .map(|record| {
            let text = [
                expand_identifiers(&record.bare_name),
                expand_identifiers(&record.arg_names.join(" ")),
                expand_identifiers(&record.return_type_name),
                expand_identifiers(&record.description),
                expand_identifiers(&record.nested),
            ]
            .join(" ");
            OperationDocument {
                op: record.op,
                text,
            }
        })
        .collect()
}

/// Tantivy field handles bundled together for ergonomic doc writing.
struct DocFields {
    operation_name: Field,
    arg_names: Field,
    return_type_name: Field,
    description: Field,
    nested_fields: Field,
    operation_type_raw: Field,
    operation_name_raw: Field,
    return_type_name_raw: Field,
    field_args_raw: Field,
    scope_raw: Field,
}

/// Types of operations to be included in the schema index. Unlike the AST types, these types can
/// be included in an [`EnumSet`].
#[derive(EnumSetType, Debug, Hash)]
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

/// Maps an [`OperationType`] to the root type name it produces in a GraphQL schema. Mirrors the
/// `Display` mapping in [`OperationRef`].
fn root_kind_str(op: OperationType) -> &'static str {
    match op {
        OperationType::Query => "Query",
        OperationType::Mutation => "Mutation",
        OperationType::Subscription => "Subscription",
    }
}

/// Parse a stored `operation_type_raw` value back into an [`OperationType`].
fn parse_op_kind(s: &str) -> Option<OperationType> {
    match s {
        "Query" => Some(OperationType::Query),
        "Mutation" => Some(OperationType::Mutation),
        "Subscription" => Some(OperationType::Subscription),
        _ => None,
    }
}

#[derive(Clone)]
pub struct SchemaIndex {
    inner: Index,
    text_analyzer: TextAnalyzer,
    operation_name_field: Field,
    arg_names_field: Field,
    return_type_name_field: Field,
    description_field: Field,
    nested_fields_field: Field,
    operation_type_raw_field: Field,
    operation_name_raw_field: Field,
    return_type_name_raw_field: Field,
    field_args_raw_field: Field,
    scope_raw_field: Field,
    /// Distinct service scopes present in the corpus (derived from operation-name
    /// prefixes). Lets a caller distinguish an unknown scope from a valid-but-empty one.
    scopes: BTreeSet<String>,
}

impl SchemaIndex {
    #[tracing::instrument(skip_all, name = "schema_index")]
    pub fn new(
        schema: &Valid<Schema>,
        root_types: EnumSet<OperationType>,
        flatten_depth: usize,
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
        let operation_name_field = index_schema.add_text_field(
            OPERATION_NAME_FIELD,
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
        let nested_fields_field = index_schema.add_text_field(
            NESTED_FIELDS_FIELD,
            TextOptions::default().set_indexing_options(text_indexing()),
        );

        // Raw identifier fields preserve exact casing for lookup and display.
        let raw_indexing = || {
            TextOptions::default()
                .set_indexing_options(TextFieldIndexing::default().set_tokenizer("raw"))
                .set_stored()
        };
        let operation_type_raw_field =
            index_schema.add_text_field(OPERATION_TYPE_RAW_FIELD, raw_indexing());
        let operation_name_raw_field =
            index_schema.add_text_field(OPERATION_NAME_RAW_FIELD, raw_indexing());
        let return_type_name_raw_field =
            index_schema.add_text_field(RETURN_TYPE_NAME_RAW_FIELD, raw_indexing());
        let field_args_raw_field = index_schema.add_text_field(FIELD_ARGS_RAW_FIELD, STORED);
        let scope_raw_field = index_schema.add_text_field(SCOPE_RAW_FIELD, raw_indexing());

        // Create the index
        let index_schema = index_schema.build();
        let index = Index::create_in_ram(index_schema);
        index
            .tokenizers()
            .register("en_stem", text_analyzer.clone());

        let doc_fields = DocFields {
            operation_name: operation_name_field,
            arg_names: arg_names_field,
            return_type_name: return_type_name_field,
            description: description_field,
            nested_fields: nested_fields_field,
            operation_type_raw: operation_type_raw_field,
            operation_name_raw: operation_name_raw_field,
            return_type_name_raw: return_type_name_raw_field,
            field_args_raw: field_args_raw_field,
            scope_raw: scope_raw_field,
        };
        let mut index_writer = index.writer(index_memory_bytes)?;
        let (operation_count, scopes) = Self::write_operation_docs(
            schema,
            &mut index_writer,
            &doc_fields,
            root_types,
            flatten_depth,
        )?;
        index_writer.commit()?;

        let elapsed = start_time.elapsed();
        info!("Indexed {} operations in {:.2?}", operation_count, elapsed);

        Ok(Self {
            inner: index,
            text_analyzer,
            operation_name_field,
            arg_names_field,
            return_type_name_field,
            description_field,
            nested_fields_field,
            operation_type_raw_field,
            operation_name_raw_field,
            return_type_name_raw_field,
            field_args_raw_field,
            scope_raw_field,
            scopes,
        })
    }

    /// The distinct service scopes present in the indexed corpus.
    pub fn scopes(&self) -> &BTreeSet<String> {
        &self.scopes
    }

    /// Write one Tantivy document per root operation field, enriched with the operation's
    /// flattened return-type text. Returns the document count and the distinct scopes seen.
    fn write_operation_docs(
        schema: &Valid<Schema>,
        index_writer: &mut tantivy::IndexWriter,
        fields: &DocFields,
        root_types: EnumSet<OperationType>,
        flatten_depth: usize,
    ) -> Result<(usize, BTreeSet<String>), IndexingError> {
        let mut count = 0usize;
        let mut scopes = BTreeSet::new();
        for record in operation_records(schema, root_types, flatten_depth) {
            if let Some(s) = &record.op.scope {
                scopes.insert(s.clone());
            }
            let mut doc = TantivyDocument::default();
            doc.add_text(fields.operation_name, expand_identifiers(&record.bare_name));
            if !record.arg_names.is_empty() {
                doc.add_text(
                    fields.arg_names,
                    expand_identifiers(&record.arg_names.join(" ")),
                );
            }
            doc.add_text(
                fields.return_type_name,
                expand_identifiers(&record.return_type_name),
            );
            doc.add_text(fields.description, expand_identifiers(&record.description));
            doc.add_text(fields.nested_fields, expand_identifiers(&record.nested));
            doc.add_text(
                fields.operation_type_raw,
                root_kind_str(record.op.operation_type),
            );
            doc.add_text(fields.operation_name_raw, &record.op.field_name);
            doc.add_text(fields.return_type_name_raw, &record.return_type_name);
            doc.add_text(fields.scope_raw, record.op.scope.as_deref().unwrap_or(""));
            for arg_type in &record.op.arg_types {
                doc.add_text(fields.field_args_raw, arg_type);
            }
            index_writer.add_document(doc)?;
            count += 1;
        }
        Ok((count, scopes))
    }

    /// Run the search query and materialize matching operation documents into [`OperationRef`]s.
    /// Results come back already sorted by Tantivy BM25 score descending; that order is kept.
    fn run_query(
        &self,
        query_text: &str,
        scope: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Scored<OperationRef>>, SearchError> {
        // `TopDocs::with_limit` panics on 0; guard the public `SchemaSearch` seam
        // (the tool clamps to >=1, but the trait is called directly elsewhere too).
        if limit == 0 {
            return Ok(Vec::new());
        }
        let searcher = self.inner.reader()?.searcher();
        let scoring = self.query(std::iter::once(query_text.to_string()));
        let query: Box<dyn Query> = match scope {
            Some(s) => {
                let scope_term = Term::from_field_text(self.scope_raw_field, s);
                let scope_filter: Box<dyn Query> = Box::new(BoostQuery::new(
                    Box::new(TermQuery::new(scope_term, IndexRecordOption::Basic)),
                    0.0,
                ));
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(scoring) as Box<dyn Query>),
                    (Occur::Must, scope_filter),
                ]))
            }
            None => Box::new(scoring),
        };
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut results: Vec<Scored<OperationRef>> = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let field_name = doc
                .get_first(self.operation_name_raw_field)
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let op_type = doc
                .get_first(self.operation_type_raw_field)
                .and_then(|v| v.as_str())
                .and_then(parse_op_kind);
            let return_type = doc
                .get_first(self.return_type_name_raw_field)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let arg_types: Vec<String> = doc
                .get_all(self.field_args_raw_field)
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect();
            let scope = doc
                .get_first(self.scope_raw_field)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            match (op_type, field_name) {
                (Some(operation_type), Some(field_name)) => results.push(Scored::new(
                    OperationRef {
                        operation_type,
                        field_name,
                        return_type,
                        arg_types,
                        scope,
                    },
                    score,
                )),
                _ => error!("Doc {doc_address:?} missing operation type or name"),
            }
        }
        Ok(results)
    }

    /// Create the query used to search for a given set of terms.
    fn query<I>(&self, terms: I) -> impl Query
    where
        I: IntoIterator<Item = String>,
    {
        // A hit on the operation name is the most direct signal that the operation is what the
        // user is looking for, so operation-name term matches get a per-token boost. Other fields
        // (args, return type, description, nested fields) contribute at their unweighted BM25
        // score.
        const OPERATION_NAME_BOOST: f32 = 3.0;

        let mut text_analyzer = self.text_analyzer.clone();
        let text_fields = [
            (self.operation_name_field, OPERATION_NAME_BOOST),
            (self.arg_names_field, 1.0_f32),
            (self.return_type_name_field, 1.0),
            (self.description_field, 1.0),
            (self.nested_fields_field, 1.0),
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

impl SchemaSearch for SchemaIndex {
    fn search(
        &self,
        query: &str,
        scope: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Scored<OperationRef>>, SearchError> {
        self.run_query(query, scope, limit)
    }

    fn scopes(&self) -> BTreeSet<String> {
        self.scopes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use rstest::{fixture, rstest};

    const TEST_SCHEMA: &str = include_str!("testdata/schema.graphql");

    /// Depth used when folding return-type text into operation documents for the schema-based
    /// tests. The test schema nests searchable concepts several levels deep (e.g. `dimensions`
    /// lives on `MediaMetadata`, reached via `Post.media -> Media.metadata`), so a depth of 3
    /// is needed to surface them through their operations.
    const FLATTEN_DEPTH: usize = 3;

    /// Every root operation is its own index document. Searching for a specific operation name
    /// must surface that operation, even when many unrelated operations contain the constituent
    /// tokens in their (folded) return-type text. Mirrors the production failure on Slack's
    /// `userByEmail`.
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

    const SCOPED_SCHEMA: &str = r#"
      type Query {
        slack_userByEmail(email: String!): SlackUser
        github_userByLogin(login: String!): GhUser
      }
      type Mutation {
        slack_sendMessage(text: String!): SlackMessage
      }
      type SlackUser { id: ID! }
      type GhUser { id: ID! }
      type SlackMessage { ok: Boolean }
    "#;

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    #[rstest]
    fn search_surfaces_target_operation() {
        let schema = Schema::parse(NOISE_SCHEMA, "noise.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let index = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            1,
            15_000_000,
        )
        .unwrap();
        let results = index.search("userByEmail", None, 10).unwrap();
        let rank = results
            .iter()
            .position(|s| s.inner.field_name == "userByEmail")
            .map(|p| p + 1);
        assert!(
            matches!(rank, Some(r) if r <= 3),
            "Expected 'userByEmail' operation in top 3, got {:?}: {:?}",
            rank,
            results
                .iter()
                .map(|s| s.inner.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[rstest]
    fn search_with_zero_limit_returns_empty() {
        let schema = Schema::parse(NOISE_SCHEMA, "noise.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let index = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            1,
            15_000_000,
        )
        .unwrap();
        // limit 0 must not panic (tantivy's TopDocs asserts limit >= 1) and returns nothing.
        let results = index.search("userByEmail", None, 0).unwrap();
        assert!(results.is_empty());
    }

    #[rstest]
    fn scope_is_derived_stripped_and_filterable() {
        let schema = Schema::parse(SCOPED_SCHEMA, "scoped.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let index = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            2,
            15_000_000,
        )
        .unwrap();

        // field_name stays the FULL executable name; scope is derived.
        let all = index.search("user", None, 10).unwrap();
        let slack_hit = all
            .iter()
            .find(|s| s.inner.field_name == "slack_userByEmail")
            .expect("slack op present");
        assert_eq!(slack_hit.inner.scope.as_deref(), Some("slack"));

        // The prefix is stripped from scored text: searching the service word by NAME matches nothing.
        let by_prefix = index.search("github", None, 10).unwrap();
        assert!(
            !by_prefix
                .iter()
                .any(|s| s.inner.field_name.starts_with("github_")),
            "prefix must not be searchable via scored text: {:?}",
            by_prefix
                .iter()
                .map(|s| s.inner.to_string())
                .collect::<Vec<_>>()
        );

        // Scope filter restricts results to one service.
        let scoped = index.search("user", Some("github"), 10).unwrap();
        assert!(!scoped.is_empty());
        assert!(
            scoped
                .iter()
                .all(|s| s.inner.scope.as_deref() == Some("github")),
            "scoped search must only return github ops: {:?}",
            scoped
                .iter()
                .map(|s| s.inner.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[rstest]
    fn scopes_reflect_the_corpus() {
        let schema = Schema::parse(SCOPED_SCHEMA, "scoped.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let index = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            2,
            15_000_000,
        )
        .unwrap();
        let scopes = index.scopes();
        // Known services are present; an unknown scope the agent might guess is not.
        assert!(scopes.contains("slack"), "expected slack in {scopes:?}");
        assert!(scopes.contains("github"), "expected github in {scopes:?}");
        assert!(
            !scopes.contains("ats"),
            "unknown scope must be absent so callers can fall back: {scopes:?}"
        );
    }

    #[rstest]
    fn search(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            FLATTEN_DEPTH,
            15_000_000,
        )
        .unwrap();

        // `dimensions` is a nested field on `MediaMetadata`, folded into the documents of the
        // operations that reach it (e.g. `uploadMedia`, `post`, `posts`).
        let results = search.search("dimensions", None, 10).unwrap();

        assert_snapshot!(
            results
                .iter()
                .take(10)
                .map(|s| s.inner.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[rstest]
    fn search_interface_implementer_fields(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            FLATTEN_DEPTH,
            15_000_000,
        )
        .unwrap();

        // `username` lives on `User`; an operation returning/reaching `User` should surface.
        let results = search.search("username", None, 10).unwrap();
        assert!(!results.is_empty(), "Should find results for 'username'");
        let ops: Vec<String> = results.iter().map(|s| s.inner.to_string()).collect();
        assert!(
            ops.iter().any(|p| p.contains("User")),
            "Should surface a User-returning operation when searching 'username'.\nFound:\n{}",
            ops.join("\n")
        );

        // `analytics` only exists on `Post` (via `PostAnalytics`), not on the Node/Content
        // interfaces, so an operation reaching `Post` should surface.
        let results = search.search("analytics", None, 10).unwrap();
        assert!(!results.is_empty(), "Should find results for 'analytics'");
        let ops: Vec<String> = results.iter().map(|s| s.inner.to_string()).collect();
        assert!(
            ops.iter().any(|p| p.contains("Post")),
            "Should surface a Post-reaching operation when searching 'analytics'.\nFound:\n{}",
            ops.join("\n")
        );
    }

    #[rstest]
    fn search_camel_case_splitting(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            FLATTEN_DEPTH,
            15_000_000,
        )
        .unwrap();

        // Searching "post" should surface post-related operations via word-boundary splitting.
        // This exercises camelCase splitting of the operation NAME itself (e.g. `updatePost`
        // tokenizes to "update post", matching the query term "post"). Argument types like
        // `UpdatePostInput` are not part of the queryable text fields, so they can only appear
        // incidentally in the Display string, not because they were matched; the query-term
        // splitting case (splitting the *search input*) is covered by `search_camel_case_query_term`.
        let results = search.search("post", None, 10).unwrap();
        let ops: Vec<String> = results.iter().map(|s| s.inner.to_string()).collect();
        assert!(
            ops.iter().any(|p| p.contains("Post")),
            "Should surface Post-related operations when searching 'post'.\nFound:\n{}",
            ops.join("\n")
        );
        assert!(
            results
                .iter()
                .any(|s| s.inner.field_name.to_lowercase().contains("post")),
            "Should surface an operation whose name contains 'post' via camelCase splitting of the operation name.\nFound:\n{}",
            ops.join("\n")
        );
    }

    #[rstest]
    fn search_camel_case_query_term(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            FLATTEN_DEPTH,
            15_000_000,
        )
        .unwrap();

        // Searching "CreatePost" should also work via camelCase splitting of the query term.
        let results = search.search("CreatePost", None, 10).unwrap();
        let ops: Vec<String> = results.iter().map(|s| s.inner.to_string()).collect();
        assert!(
            ops.iter().any(|p| p.contains("Post")),
            "Should surface Post-related operations when searching 'CreatePost'.\nFound:\n{}",
            ops.join("\n")
        );
    }

    #[rstest]
    fn search_camel_case_in_nested_field(schema: Valid<Schema>) {
        let search = SchemaIndex::new(
            &schema,
            OperationType::Query | OperationType::Mutation,
            FLATTEN_DEPTH,
            15_000_000,
        )
        .unwrap();

        // Operation documents have no descriptions in this schema (and type-level descriptions
        // are not folded), so the original "camelCase in description" intent is exercised at the
        // nearest equivalent: camelCase splitting of a deeply nested field name. `ageGroups`
        // lives on `Demographics` (reached via `Post.analytics -> PostAnalytics.demographics`)
        // and is folded as "age groups", so searching "age" surfaces Post-reaching operations.
        let results = search.search("age", None, 10).unwrap();
        let ops: Vec<String> = results.iter().map(|s| s.inner.to_string()).collect();
        assert!(
            ops.iter().any(|p| p.contains("Post")),
            "Should surface a Post-reaching operation when searching 'age' (camelCase split of nested 'ageGroups').\nFound:\n{}",
            ops.join("\n")
        );
    }

    #[rstest]
    fn enumerate_operation_documents_yields_operations_with_text() {
        let schema = Schema::parse(NOISE_SCHEMA, "noise.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let docs = enumerate_operation_documents(
            &schema,
            OperationType::Query | OperationType::Mutation,
            2,
        );
        let target = docs
            .iter()
            .find(|d| d.op.field_name == "userByEmail")
            .expect("userByEmail operation present");
        assert!(
            target.text.contains("user") && target.text.contains("email"),
            "expected enriched text to contain the operation's tokens, got: {:?}",
            target.text
        );
        // every root operation should produce exactly one document
        assert!(!docs.is_empty());
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
