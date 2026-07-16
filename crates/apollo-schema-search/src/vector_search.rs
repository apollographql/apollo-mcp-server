//! Semantic [`SchemaSearch`] backend: embeds the shared operation corpus once at build time,
//! then embeds each query and ranks operations by cosine similarity (scope-filtered).

use crate::{Embedder, InMemoryVectorStore, VectorStore};
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_schema_index::error::SearchError;
use apollo_schema_index::{
    OperationRef, OperationType, SchemaSearch, Scored, enumerate_operation_documents,
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
    pub fn build(
        schema: &Valid<Schema>,
        root_types: EnumSet<OperationType>,
        flatten_depth: usize,
        embedder: Arc<dyn Embedder>,
    ) -> Result<Self, crate::EmbedError> {
        let docs = enumerate_operation_documents(schema, root_types, flatten_depth);
        let texts: Vec<String> = docs.iter().map(|d| d.text.clone()).collect();
        let vectors = embedder.embed(&texts)?;
        let mut store = InMemoryVectorStore::new();
        // `enumerate_operation_documents` yields each operation exactly once, and this loop
        // consumes that iterator into a freshly-created store, so each operation is inserted
        // exactly once here (InMemoryVectorStore::upsert is insert-only — see Task 1 note).
        for (doc, vector) in docs.into_iter().zip(vectors) {
            store.upsert(doc.op, vector);
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
}
