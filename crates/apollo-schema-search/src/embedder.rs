#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding model init failed: {0}")]
    Init(String),
    #[error("embedding inference failed: {0}")]
    Inference(String),
}

/// Turns text into dense vectors. Implemented by the fastembed model (prod) and
/// a deterministic fake (tests).
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
    fn dimensions(&self) -> usize;
}

/// Deterministic, offline embedder for tests: hashes tokens into a fixed-dim vector.
/// Not semantically meaningful — only stable and dependency-free.
pub struct FakeEmbedder {
    dims: usize,
}

impl FakeEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dims];
                for tok in t.split_whitespace() {
                    let h = tok
                        .bytes()
                        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
                    let idx = (h as usize) % self.dims;
                    // indexing guarded by modulo; use get_mut to satisfy indexing_slicing lint
                    if let Some(slot) = v.get_mut(idx) {
                        *slot += 1.0;
                    }
                }
                v
            })
            .collect())
    }
    fn dimensions(&self) -> usize {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fake_is_deterministic_and_right_dim() {
        let e = FakeEmbedder::new(8);
        let a = e.embed(&["send message".into()]).unwrap();
        let b = e.embed(&["send message".into()]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), 8);
    }
}
