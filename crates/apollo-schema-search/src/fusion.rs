use apollo_schema_index::{OperationRef, Scored};

/// Reciprocal Rank Fusion: score(op) = Σ 1/(k + rank_in_list). Rank-based, no
/// score normalization. Input lists are each already sorted best-first.
pub fn rrf_fuse(lists: &[Vec<Scored<OperationRef>>], k: f32) -> Vec<Scored<OperationRef>> {
    use std::collections::HashMap;
    let mut acc: HashMap<OperationRef, f32> = HashMap::new();
    for list in lists {
        for (rank, scored) in list.iter().enumerate() {
            *acc.entry(scored.inner.clone()).or_insert(0.0) += 1.0 / (k + rank as f32 + 1.0);
        }
    }
    let mut out: Vec<Scored<OperationRef>> =
        acc.into_iter().map(|(op, s)| Scored::new(op, s)).collect();
    out.sort_by(|a, b| {
        b.score()
            .partial_cmp(&a.score())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.inner.to_string().cmp(&b.inner.to_string()))
    });
    out
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

    #[test]
    fn fuses_by_rank_rewarding_agreement() {
        // `a` is top of both lists → should win over `b`/`c` that each rank high in only one.
        let l1 = vec![Scored::new(op("a"), 9.0), Scored::new(op("b"), 8.0)];
        let l2 = vec![Scored::new(op("a"), 0.5), Scored::new(op("c"), 0.4)];
        let fused = rrf_fuse(&[l1, l2], 60.0);
        assert_eq!(fused.first().unwrap().inner.field_name, "a");
        assert_eq!(fused.len(), 3); // a, b, c deduped
    }

    #[test]
    fn single_list_preserves_order() {
        let l1 = vec![Scored::new(op("x"), 5.0), Scored::new(op("y"), 4.0)];
        let fused = rrf_fuse(&[l1], 60.0);
        assert_eq!(fused[0].inner.field_name, "x");
        assert_eq!(fused[1].inner.field_name, "y");
    }

    #[test]
    fn ties_break_deterministically_by_display_order() {
        // Both `b` and `a` rank #1 in their respective single-item lists, so they
        // end up with an identical fused score. Without a tiebreaker, the order
        // would depend on HashMap iteration order (non-deterministic).
        for _ in 0..10 {
            let l1 = vec![Scored::new(op("b"), 1.0)];
            let l2 = vec![Scored::new(op("a"), 1.0)];
            let fused = rrf_fuse(&[l1, l2], 60.0);
            assert_eq!(fused.len(), 2);
            assert_eq!(fused[0].score(), fused[1].score(), "scores should be tied");
            // Alphabetical-by-Display order: "Query.a" < "Query.b".
            assert_eq!(fused[0].inner.field_name, "a");
            assert_eq!(fused[1].inner.field_name, "b");
        }
    }
}
