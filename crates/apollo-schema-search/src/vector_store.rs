use apollo_schema_index::{OperationRef, Scored};

pub trait VectorStore: Send + Sync {
    fn upsert(&mut self, op: OperationRef, vector: Vec<f32>);
    fn search(&self, query: &[f32], scope: Option<&str>, limit: usize)
    -> Vec<Scored<OperationRef>>;
}

#[derive(Default)]
pub struct InMemoryVectorStore {
    items: Vec<(OperationRef, Vec<f32>)>,
}

impl InMemoryVectorStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl VectorStore for InMemoryVectorStore {
    fn upsert(&mut self, op: OperationRef, mut vector: Vec<f32>) {
        normalize(&mut vector);
        self.items.push((op, vector));
    }

    fn search(
        &self,
        query: &[f32],
        scope: Option<&str>,
        limit: usize,
    ) -> Vec<Scored<OperationRef>> {
        if limit == 0 {
            return Vec::new();
        }
        let mut q = query.to_vec();
        normalize(&mut q);
        let mut scored: Vec<Scored<OperationRef>> = self
            .items
            .iter()
            .filter(|(op, _)| scope.is_none_or(|s| op.scope.as_deref() == Some(s)))
            .map(|(op, v)| Scored::new(op.clone(), dot(&q, v)))
            .collect();
        scored.sort_by(|a, b| {
            b.score()
                .partial_cmp(&a.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_schema_index::OperationType;

    fn op(name: &str, scope: Option<&str>) -> OperationRef {
        OperationRef {
            operation_type: OperationType::Query,
            field_name: name.into(),
            return_type: None,
            arg_types: vec![],
            scope: scope.map(str::to_string),
        }
    }

    #[test]
    fn returns_nearest_by_cosine() {
        let mut s = InMemoryVectorStore::new();
        s.upsert(op("a", None), vec![1.0, 0.0]);
        s.upsert(op("b", None), vec![0.0, 1.0]);
        let r = s.search(&[0.9, 0.1], None, 10);
        assert_eq!(r[0].inner.field_name, "a");
    }

    #[test]
    fn scope_prefilters() {
        let mut s = InMemoryVectorStore::new();
        s.upsert(op("slack_x", Some("slack")), vec![1.0, 0.0]);
        s.upsert(op("ashby_y", Some("ashby")), vec![1.0, 0.0]);
        let r = s.search(&[1.0, 0.0], Some("slack"), 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].inner.scope.as_deref(), Some("slack"));
    }

    #[test]
    fn zero_limit_empty() {
        let s = InMemoryVectorStore::new();
        assert!(s.search(&[1.0], None, 0).is_empty());
    }
}
