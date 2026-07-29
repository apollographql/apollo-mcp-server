//! Fuses multiple [`SchemaSearch`] backends (e.g. lexical BM25 + semantic vector
//! search) into a single ranked result set via reciprocal rank fusion.

use crate::rrf_fuse;
use apollo_schema_index::error::SearchError;
use apollo_schema_index::{OperationRef, SchemaSearch, Scored};
use std::collections::BTreeSet;
use tracing::warn;

/// Candidate pool pulled from each backend before fusion. Large enough that a
/// good result present in only one backend still survives into the fused top-k.
const CANDIDATE_POOL: usize = 50;

/// Fuses multiple `SchemaSearch` backends (e.g. lexical + semantic) with RRF.
pub struct HybridSearch {
    backends: Vec<Box<dyn SchemaSearch + Send + Sync>>,
    rrf_k: f32,
}

impl HybridSearch {
    pub fn new(backends: Vec<Box<dyn SchemaSearch + Send + Sync>>, rrf_k: f32) -> Self {
        Self { backends, rrf_k }
    }
}

impl SchemaSearch for HybridSearch {
    fn search(
        &self,
        query: &str,
        scope: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Scored<OperationRef>>, SearchError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let pool = limit.max(CANDIDATE_POOL);
        let mut lists: Vec<Vec<Scored<OperationRef>>> = Vec::new();
        for backend in &self.backends {
            match backend.search(query, scope, pool) {
                Ok(list) => lists.push(list),
                // Degradation: a failing backend (e.g. embedder error) is skipped,
                // not fatal — the remaining backends still produce results.
                Err(e) => warn!("hybrid: a search backend failed, skipping it: {e}"),
            }
        }
        let mut fused = rrf_fuse(&lists, self.rrf_k);
        fused.truncate(limit);
        Ok(fused)
    }

    /// Union of the fused backends' scopes (in practice the lexical index supplies
    /// them; the vector backend returns an empty set).
    fn scopes(&self) -> BTreeSet<String> {
        self.backends.iter().flat_map(|b| b.scopes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_schema_index::OperationType;

    fn op(name: &str) -> OperationRef {
        OperationRef {
            operation_type: OperationType::Query,
            field_name: name.into(),
            return_type: None,
            arg_types: vec![],
            scope: None,
        }
    }

    /// A canned-results backend. `Scored` doesn't implement `Clone`, so results
    /// are rebuilt from their parts rather than cloned wholesale.
    struct Stub(Vec<Scored<OperationRef>>);
    impl SchemaSearch for Stub {
        fn search(
            &self,
            _query: &str,
            _scope: Option<&str>,
            limit: usize,
        ) -> Result<Vec<Scored<OperationRef>>, SearchError> {
            Ok(self
                .0
                .iter()
                .take(limit)
                .map(|s| Scored::new(s.inner.clone(), s.score()))
                .collect())
        }
    }

    struct Failing;
    impl SchemaSearch for Failing {
        fn search(
            &self,
            _query: &str,
            _scope: Option<&str>,
            _limit: usize,
        ) -> Result<Vec<Scored<OperationRef>>, SearchError> {
            Err(SearchError::Embedding("boom".into()))
        }
    }

    #[test]
    fn fuses_agreement_to_the_top() {
        // `a` is top of both backends → should be #1 after fusion.
        let b1 = Box::new(Stub(vec![
            Scored::new(op("a"), 9.0),
            Scored::new(op("b"), 8.0),
        ])) as Box<dyn SchemaSearch + Send + Sync>;
        let b2 = Box::new(Stub(vec![
            Scored::new(op("a"), 0.9),
            Scored::new(op("c"), 0.8),
        ])) as Box<dyn SchemaSearch + Send + Sync>;
        let hs = HybridSearch::new(vec![b1, b2], 60.0);
        let r = hs.search("x", None, 10).unwrap();
        assert_eq!(r.first().unwrap().inner.field_name, "a");
    }

    #[test]
    fn skips_failing_backend() {
        let ok =
            Box::new(Stub(vec![Scored::new(op("a"), 1.0)])) as Box<dyn SchemaSearch + Send + Sync>;
        let bad = Box::new(Failing) as Box<dyn SchemaSearch + Send + Sync>;
        let hs = HybridSearch::new(vec![ok, bad], 60.0);
        let r = hs.search("x", None, 10).unwrap(); // must not error
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].inner.field_name, "a");
    }

    #[test]
    fn respects_limit_and_zero() {
        let b = Box::new(Stub(vec![
            Scored::new(op("a"), 3.0),
            Scored::new(op("b"), 2.0),
            Scored::new(op("c"), 1.0),
        ])) as Box<dyn SchemaSearch + Send + Sync>;
        let hs = HybridSearch::new(vec![b], 60.0);
        assert_eq!(hs.search("x", None, 2).unwrap().len(), 2);
        assert!(hs.search("x", None, 0).unwrap().is_empty());
    }
}
