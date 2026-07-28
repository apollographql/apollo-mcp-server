//! MCP tool to search a GraphQL schema.

use crate::errors::McpError;
use crate::introspection::minify::MinifyExt as _;
use crate::schema_from_type;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::ast::{Field, OperationType as AstOperationType, Selection};
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Node, Schema};
use apollo_schema_index::{OperationType, SchemaIndex, SchemaSearch};
use apollo_schema_search::{
    DOC_BUILDER_VERSION, Embedder, EmbeddingStore, FastembedEmbedder, HybridSearch, PostgresCache,
    VectorSearch,
};
use rmcp::model::{CallToolResult, Content, ErrorCode, Tool};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

use super::description::append_description_hint;

/// The name of the tool to search a GraphQL schema.
pub const SEARCH_TOOL_NAME: &str = "search";

/// A tool to search a GraphQL schema.
#[derive(Clone)]
pub struct Search {
    schema: Arc<RwLock<Valid<Schema>>>,
    /// Composed backend: `HybridSearch[lexical, semantic]` when an embedder is
    /// present, otherwise the bare lexical `SchemaIndex`. Both impl `SchemaSearch`.
    search: Arc<RwLock<Box<dyn SchemaSearch + Send + Sync>>>,
    allow_mutations: bool,
    leaf_depth: usize,
    flatten_depth: usize,
    index_memory_bytes: usize,
    default_limit: usize,
    max_limit: usize,
    minify: bool,
    /// Retained so `rebuild` can reconstruct the vector index WITHOUT re-loading
    /// the model. `None` = lexical-only (semantic disabled or embedder init failed).
    embedder: Option<Arc<dyn Embedder>>,
    rrf_k: f32,
    /// Optional Postgres URL for the shared embedding cache, retained so `rebuild`
    /// can reopen it (fail-open) across rebuilds without re-embedding unchanged docs.
    cache_url: Option<String>,
    model_id: String,
    pub tool: Tool,
}

/// Input for the search tool.
#[derive(JsonSchema, Deserialize, Debug)]
pub struct Input {
    /// The search terms as a short natural-language phrase expressing ONE intent,
    /// including the high-signal domain nouns you expect in the target operation
    /// or type name (e.g. "incident severity status", "send a direct message to a
    /// Slack user"). Group concepts from the same domain into one query; put
    /// concepts from different domains in separate search calls. The semantic arm
    /// reads intent; the lexical arm reads the nouns — so both a phrase and its key
    /// terms help. (Passed as one or more strings, joined into a single query.)
    terms: Vec<String>,
    /// Maximum number of results to return (default 10, max 50).
    #[serde(default)]
    limit: Option<usize>,
    /// Restrict the search to a single subgraph/service. Use only to disambiguate a
    /// broad term that spans domains; leave unset otherwise so ranking spans all
    /// services.
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

    #[error("Search index build task failed: {0}")]
    BuildJoin(#[from] tokio::task::JoinError),
}

/// Clamp the requested result count to `[1, max_limit]`, defaulting when omitted.
fn clamp_limit(requested: Option<usize>, default_limit: usize, max_limit: usize) -> usize {
    requested.unwrap_or(default_limit).clamp(1, max_limit)
}

/// Open the Postgres embedding cache, or `None` when no URL is configured or the
/// connection fails (fail-open: the caller embeds from scratch).
fn open_store(cache_url: Option<&str>, model_id: &str, dim: usize) -> Option<PostgresCache> {
    let url = cache_url?;
    match PostgresCache::open(url, model_id, dim, DOC_BUILDER_VERSION) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("embedding cache disabled (postgres connect failed): {e}");
            None
        }
    }
}

/// Build the search backend from a schema. With an embedder, composes
/// `HybridSearch[lexical, semantic]`; if the semantic index fails to build,
/// logs a warning and degrades to lexical-only. Without an embedder, returns
/// the bare lexical index. Both branches yield a `SchemaSearch`.
#[allow(clippy::too_many_arguments)]
fn build_backend(
    schema: &Valid<Schema>,
    allow_mutations: bool,
    flatten_depth: usize,
    index_memory_bytes: usize,
    embedder: Option<Arc<dyn Embedder>>,
    rrf_k: f32,
    cache_url: Option<&str>,
    model_id: &str,
) -> Result<Box<dyn SchemaSearch + Send + Sync>, IndexingError> {
    let root_types = if allow_mutations {
        OperationType::Query | OperationType::Mutation
    } else {
        OperationType::Query.into()
    };
    let index = SchemaIndex::new(schema, root_types, flatten_depth, index_memory_bytes)?;
    match embedder {
        Some(emb) => {
            // Open the cache if configured; a failure is non-fatal (embed from scratch).
            let mut cache = open_store(cache_url, model_id, emb.dimensions());
            match VectorSearch::build(
                schema,
                root_types,
                flatten_depth,
                emb,
                cache.as_mut().map(|c| c as &mut dyn EmbeddingStore),
            ) {
                Ok(vector) => Ok(Box::new(HybridSearch::new(
                    vec![Box::new(index), Box::new(vector)],
                    rrf_k,
                ))),
                Err(e) => {
                    warn!("semantic index build failed; degrading to lexical-only: {e}");
                    Ok(Box::new(index))
                }
            }
        }
        None => Ok(Box::new(index)),
    }
}

impl Search {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        schema: Arc<RwLock<Valid<Schema>>>,
        allow_mutations: bool,
        leaf_depth: usize,
        flatten_depth: usize,
        index_memory_bytes: usize,
        default_limit: usize,
        max_limit: usize,
        minify: bool,
        description_hint: Option<&str>,
        semantic_enabled: bool,
        semantic_model: &str,
        semantic_inference_threads: usize,
        rrf_k: f32,
        semantic_cache_url: Option<String>,
    ) -> Result<Self, IndexingError> {
        // Model load, embedding, and Postgres I/O are all blocking — and the sync
        // Postgres client cannot even be created on a runtime thread — so run the
        // whole construction off the async runtime.
        let description_hint = description_hint.map(str::to_string);
        let semantic_model = semantic_model.to_string();
        tokio::task::spawn_blocking(move || {
            let embedder: Option<Arc<dyn Embedder>> = if semantic_enabled {
                match FastembedEmbedder::new(&semantic_model, semantic_inference_threads) {
                    Ok(e) => Some(Arc::new(e)),
                    Err(e) => {
                        warn!("embedder init failed; semantic search disabled (lexical-only): {e}");
                        None
                    }
                }
            } else {
                None
            };
            Self::new_with_embedder(
                schema,
                allow_mutations,
                leaf_depth,
                flatten_depth,
                index_memory_bytes,
                default_limit,
                max_limit,
                minify,
                description_hint.as_deref(),
                embedder,
                rrf_k,
                &semantic_model,
                semantic_cache_url,
            )
        })
        .await?
    }

    /// Core constructor. Takes an already-resolved embedder (or `None`), so tests
    /// can inject a `FakeEmbedder`/failing stub and exercise hybrid + degradation
    /// paths offline. Production goes through `new`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_embedder(
        schema: Arc<RwLock<Valid<Schema>>>,
        allow_mutations: bool,
        leaf_depth: usize,
        flatten_depth: usize,
        index_memory_bytes: usize,
        default_limit: usize,
        max_limit: usize,
        minify: bool,
        description_hint: Option<&str>,
        embedder: Option<Arc<dyn Embedder>>,
        rrf_k: f32,
        model_id: &str,
        cache_url: Option<String>,
    ) -> Result<Self, IndexingError> {
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
        let backend = build_backend(
            locked,
            allow_mutations,
            flatten_depth,
            index_memory_bytes,
            embedder.clone(),
            rrf_k,
            cache_url.as_deref(),
            model_id,
        )?;
        Ok(Self {
            schema: schema.clone(),
            search: Arc::new(RwLock::new(backend)),
            allow_mutations,
            leaf_depth,
            flatten_depth,
            index_memory_bytes,
            default_limit,
            max_limit,
            minify,
            embedder,
            rrf_k,
            cache_url,
            model_id: model_id.to_string(),
            tool: Tool::new(SEARCH_TOOL_NAME, description, schema_from_type!(Input)),
        })
    }

    /// Rebuild the search index from an updated schema, replacing the current index.
    pub async fn rebuild(&self, schema: &Valid<Schema>) -> Result<(), IndexingError> {
        // Embedding + Postgres I/O are blocking; run off the async runtime.
        let schema = schema.clone();
        let allow_mutations = self.allow_mutations;
        let flatten_depth = self.flatten_depth;
        let index_memory_bytes = self.index_memory_bytes;
        let embedder = self.embedder.clone();
        let rrf_k = self.rrf_k;
        let cache_url = self.cache_url.clone();
        let model_id = self.model_id.clone();
        let backend = tokio::task::spawn_blocking(move || {
            build_backend(
                &schema,
                allow_mutations,
                flatten_depth,
                index_memory_bytes,
                embedder,
                rrf_k,
                cache_url.as_deref(),
                &model_id,
            )
        })
        .await??;
        *self.search.write().await = backend;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
        let k = clamp_limit(input.limit, self.default_limit, self.max_limit);
        let query = input.terms.join(" ");
        let scope = input.scope.clone();
        let search = self.search.clone();
        let results = tokio::task::spawn_blocking(move || {
            let guard = search.blocking_read();
            // Validate the requested scope against the corpus. Scope is derived from
            // operation-name prefixes (e.g. "slack", "ashby"). If the agent guesses a
            // scope that isn't a real service (e.g. "ats" when it's "ashby"), the
            // scoped query would filter out everything and return nothing — wasting a
            // round-trip while the agent re-searches unscoped. So an *unknown* scope is
            // dropped and we search globally. A *known* scope is honored even when it
            // yields no matches for this particular query.
            let effective = match scope.as_deref() {
                Some(s) if !guard.scopes().contains(s) => {
                    warn!(
                        scope = s,
                        "unknown scope (not a service in the schema); searching all services"
                    );
                    None
                }
                other => other,
            };
            guard.search(&query, effective, k)
        })
        .await
        .map_err(|e| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Search task join failed: {e}"),
                None,
            )
        })?
        .map_err(|e| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to search index: {e}"),
                None,
            )
        })?;

        let schema = self.schema.read().await;
        let mut tree_shaker = SchemaTreeShaker::new(&schema);
        for scored in results {
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

    struct FailingEmbedder;
    impl apollo_schema_search::Embedder for FailingEmbedder {
        fn embed(
            &self,
            _texts: &[String],
        ) -> Result<Vec<Vec<f32>>, apollo_schema_search::EmbedError> {
            Err(apollo_schema_search::EmbedError::Inference("boom".into()))
        }
        fn dimensions(&self) -> usize {
            384
        }
    }

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
        let search = Search::new_with_embedder(
            schema.clone(),
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            None,
            60.0,
            "fake",
            None,
        )
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
        let search = Search::new_with_embedder(
            schema.clone(),
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            None,
            60.0,
            "fake",
            None,
        )
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
    async fn unknown_scope_falls_back_to_global(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let make = || {
            Search::new_with_embedder(
                schema.clone(),
                false,
                1,
                2,
                15_000_000,
                10,
                50,
                false,
                None,
                None,
                60.0,
                "fake",
                None,
            )
            .expect("Failed to create search tool")
        };

        let global = make()
            .execute(Input {
                terms: vec!["User".to_string()],
                limit: None,
                scope: None,
            })
            .await
            .expect("global search failed");
        let bogus = make()
            .execute(Input {
                terms: vec!["User".to_string()],
                limit: None,
                scope: Some("no-such-service".to_string()),
            })
            .await
            .expect("scoped search failed");

        let global = content_to_snapshot(global);
        let bogus = content_to_snapshot(bogus);
        assert!(
            !global.trim().is_empty(),
            "fixture should return results for the global search"
        );
        assert_eq!(
            bogus, global,
            "an unknown scope must fall back to the global result, not return empty"
        );
    }

    #[rstest]
    #[tokio::test]
    async fn referencing_types_are_collected(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new_with_embedder(
            schema.clone(),
            true,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            None,
            60.0,
            "fake",
            None,
        )
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
        let search = Search::new_with_embedder(
            schema.clone(),
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            None,
            60.0,
            "fake",
            None,
        )
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
        let search = Search::new_with_embedder(
            schema.clone(),
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            true,
            None,
            None,
            60.0,
            "fake",
            None,
        )
        .expect("Failed to create search tool");

        let description = search.tool.description.unwrap();

        // Should contain minification legend
        assert!(description.contains("T=type,I=input,E=enum,U=union,F=interface"));
        assert!(description.contains("s=String,i=Int,f=Float,b=Boolean,d=ID"));
    }

    #[rstest]
    #[tokio::test]
    async fn degrades_to_lexical_when_embedder_fails(schema: Valid<Schema>) {
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new_with_embedder(
            schema.clone(),
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            Some(Arc::new(FailingEmbedder)),
            60.0,
            "fake",
            None,
        )
        .expect("tool must still build when the embedder fails");
        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
                limit: None,
                scope: None,
            })
            .await
            .expect("search must still execute (lexical-only)");
        assert!(!result.is_error.unwrap_or(false));
        assert!(
            !content_to_snapshot(result).is_empty(),
            "lexical fallback should return results"
        );
    }

    #[rstest]
    #[tokio::test]
    async fn hybrid_with_fake_embedder_returns_results(schema: Valid<Schema>) {
        use apollo_schema_search::FakeEmbedder;
        let schema = Arc::new(RwLock::new(schema));
        let search = Search::new_with_embedder(
            schema.clone(),
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            Some(Arc::new(FakeEmbedder::new(384))),
            60.0,
            "fake",
            None,
        )
        .expect("hybrid tool must build");
        let result = search
            .execute(Input {
                terms: vec!["User".to_string()],
                limit: None,
                scope: None,
            })
            .await
            .expect("hybrid search must execute");
        assert!(!result.is_error.unwrap_or(false));
        assert!(!content_to_snapshot(result).is_empty());
    }

    #[test]
    fn open_store_none_when_no_url() {
        assert!(open_store(None, "m", 8).is_none());
    }

    #[test]
    fn open_store_bad_url_is_fail_open_none() {
        // Unreachable/garbage URL must fail open to None, never panic.
        let url = "host=127.0.0.1 port=1 user=nope dbname=nope connect_timeout=1";
        assert!(open_store(Some(url), "m", 4).is_none());
    }

    #[rstest]
    fn build_is_fail_open_when_cache_unreachable(schema: Valid<Schema>) {
        // Sync test: with no ambient tokio runtime the blocking Postgres client can
        // be constructed directly (in production this path runs via spawn_blocking).
        // An unreachable URL must fail open — the tool still builds, degrading to
        // no-cache, never erroring or panicking.
        let schema = Arc::new(RwLock::new(schema));
        let result = Search::new_with_embedder(
            schema,
            false,
            1,
            2,
            15_000_000,
            10,
            50,
            false,
            None,
            Some(Arc::new(apollo_schema_search::FakeEmbedder::new(64))),
            60.0,
            "fake",
            Some("host=127.0.0.1 port=1 user=nope dbname=nope connect_timeout=1".to_string()),
        );
        assert!(
            result.is_ok(),
            "tool must build even when the cache is unreachable"
        );
    }
}
