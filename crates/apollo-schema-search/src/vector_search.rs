//! Semantic [`SchemaSearch`] backend: embeds the shared operation corpus once at build time,
//! then embeds each query and ranks operations by cosine similarity (scope-filtered).

use crate::embedding_cache::{EmbeddingCache, doc_key};
use crate::{Embedder, InMemoryVectorStore, VectorStore};
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_schema_index::error::SearchError;
use apollo_schema_index::{
    OperationDocument, OperationRef, OperationType, SchemaSearch, Scored,
    enumerate_operation_documents,
};
use enumset::EnumSet;
use std::sync::Arc;

/// Vector (cosine similarity) search over the same operation corpus [`apollo_schema_index::SchemaIndex`]
/// (BM25) indexes, embedded offline via [`Embedder`].
pub struct VectorSearch {
    store: InMemoryVectorStore,
    embedder: Arc<dyn Embedder>,
}

impl VectorSearch {
    /// Build the vector index: enumerate operation documents (the SAME corpus BM25 uses),
    /// embed each document's text in one batch, and insert one vector per operation.
    #[tracing::instrument(skip_all, name = "embedding")]
    pub fn build(
        schema: &Valid<Schema>,
        root_types: EnumSet<OperationType>,
        flatten_depth: usize,
        embedder: Arc<dyn Embedder>,
        cache: Option<&mut EmbeddingCache>,
    ) -> Result<Self, crate::EmbedError> {
        let docs = enumerate_operation_documents(schema, root_types, flatten_depth);
        let mut store = InMemoryVectorStore::new();
        let mut miss_keys: Vec<String> = Vec::new();
        let mut miss_docs: Vec<OperationDocument> = Vec::new();
        let mut reused = 0usize;

        // 1. Content-addressed lookup: reuse cached vectors, collect misses.
        for doc in docs {
            let key = doc_key(&doc.text);
            let hit = match cache.as_deref() {
                Some(c) => c.get(&key).unwrap_or(None), // a cache read error == a miss
                None => None,
            };
            match hit {
                Some(vector) => {
                    store.upsert(doc.op, vector);
                    reused += 1;
                }
                None => {
                    miss_keys.push(key);
                    miss_docs.push(doc);
                }
            }
        }

        // 2. Embed only the misses (the expensive step), persist, and store.
        if miss_docs.is_empty() {
            tracing::info!(reused, "Loaded all embeddings from cache");
        } else {
            let texts: Vec<String> = miss_docs.iter().map(|d| d.text.clone()).collect();
            let start = std::time::Instant::now();
            let vectors = embedder.embed(&texts)?;
            tracing::info!(
                embedded = texts.len(),
                reused,
                "Embedded corpus in {:.2?}",
                start.elapsed()
            );
            if let Some(c) = cache {
                let entries: Vec<(String, Vec<f32>)> =
                    miss_keys.into_iter().zip(vectors.iter().cloned()).collect();
                if let Err(e) = c.put_batch(&entries) {
                    tracing::warn!("failed to persist embeddings to cache: {e}");
                }
            }
            for (doc, vector) in miss_docs.into_iter().zip(vectors) {
                store.upsert(doc.op, vector);
            }
        }
        Ok(Self { store, embedder })
    }
}

impl SchemaSearch for VectorSearch {
    fn search(
        &self,
        query: &str,
        scope: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Scored<OperationRef>>, SearchError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let vectors = self
            .embedder
            .embed(&[query.to_string()])
            .map_err(|e| SearchError::Embedding(e.to_string()))?;
        let query_vec = vectors.into_iter().next().ok_or_else(|| {
            SearchError::Embedding("embedder returned no vector for query".into())
        })?;
        Ok(self.store.search(&query_vec, scope, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeEmbedder;
    use apollo_compiler::Schema;

    const SCHEMA: &str = r#"
        type Query {
          slack_userByEmail(email: String!): SlackUser
          github_userByLogin(login: String!): GhUser
        }
        type SlackUser { id: ID! }
        type GhUser { id: ID! }
    "#;

    fn index() -> VectorSearch {
        let schema = Schema::parse(SCHEMA, "s.graphql")
            .unwrap()
            .validate()
            .unwrap();
        VectorSearch::build(
            &schema,
            OperationType::Query | OperationType::Mutation,
            2,
            Arc::new(FakeEmbedder::new(64)),
            None,
        )
        .unwrap()
    }

    #[test]
    fn returns_operations_and_carries_scope() {
        let vs = index();
        let results = vs.search("user by email", None, 10).unwrap();
        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .any(|s| s.inner.field_name == "slack_userByEmail")
        );
        let slack = results
            .iter()
            .find(|s| s.inner.field_name == "slack_userByEmail")
            .unwrap();
        assert_eq!(slack.inner.scope.as_deref(), Some("slack"));
    }

    #[test]
    fn scope_restricts_results() {
        let vs = index();
        let results = vs.search("user", Some("github"), 10).unwrap();
        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .all(|s| s.inner.scope.as_deref() == Some("github"))
        );
    }

    #[test]
    fn zero_limit_is_empty() {
        assert!(index().search("user", None, 0).unwrap().is_empty());
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingEmbedder {
        inner: FakeEmbedder,
        embedded: Arc<AtomicUsize>,
    }
    impl Embedder for CountingEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, crate::EmbedError> {
            self.embedded.fetch_add(texts.len(), Ordering::SeqCst);
            self.inner.embed(texts)
        }
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }
    }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "vs_cache_{}_{}_{}.db",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn second_build_reuses_cache_and_embeds_nothing() {
        let schema = Schema::parse(SCHEMA, "s.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let roots = OperationType::Query | OperationType::Mutation;
        let path = tmp_db("reuse");

        // First build: cold cache -> embeds all ops.
        let count1 = Arc::new(AtomicUsize::new(0));
        {
            let mut cache = EmbeddingCache::open(&path, "fake", 64, 1).unwrap();
            let emb = Arc::new(CountingEmbedder {
                inner: FakeEmbedder::new(64),
                embedded: count1.clone(),
            });
            VectorSearch::build(&schema, roots, 2, emb, Some(&mut cache)).unwrap();
        }
        assert!(
            count1.load(Ordering::SeqCst) > 0,
            "first build should embed"
        );

        // Second build: warm cache -> embeds nothing, still returns results.
        let count2 = Arc::new(AtomicUsize::new(0));
        let vs = {
            let mut cache = EmbeddingCache::open(&path, "fake", 64, 1).unwrap();
            let emb = Arc::new(CountingEmbedder {
                inner: FakeEmbedder::new(64),
                embedded: count2.clone(),
            });
            VectorSearch::build(&schema, roots, 2, emb, Some(&mut cache)).unwrap()
        };
        assert_eq!(
            count2.load(Ordering::SeqCst),
            0,
            "second build must be all cache hits"
        );
        assert!(!vs.search("user by email", None, 10).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
