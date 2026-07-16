use crate::embedder::{EmbedError, Embedder};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::sync::Mutex;

/// Production [`Embedder`] backed by `fastembed`/ONNX Runtime.
///
/// `TextEmbedding::embed` takes `&mut self`, so the model is guarded by a
/// [`Mutex`] to satisfy the `Send + Sync` bound on [`Embedder`].
pub struct FastembedEmbedder {
    model: Mutex<TextEmbedding>,
    dims: usize,
}

impl FastembedEmbedder {
    /// `model_name` e.g. "bge-small-en-v1.5". `inference_threads` pins ORT's intra-op
    /// thread pool (keep small, e.g. 1, to respect container CPU limits).
    pub fn new(model_name: &str, inference_threads: usize) -> Result<Self, EmbedError> {
        let (model, dims) = resolve_model(model_name)?;
        let embedding = TextEmbedding::try_new(
            TextInitOptions::new(model)
                .with_show_download_progress(false)
                .with_intra_threads(inference_threads),
        )
        .map_err(|e| EmbedError::Init(e.to_string()))?;
        Ok(Self {
            model: Mutex::new(embedding),
            dims,
        })
    }
}

impl Embedder for FastembedEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut guard = self
            .model
            .lock()
            .map_err(|e| EmbedError::Inference(format!("embedder mutex poisoned: {e}")))?;
        guard
            .embed(texts, None)
            .map_err(|e| EmbedError::Inference(e.to_string()))
    }
    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// Map a model name to the fastembed [`EmbeddingModel`] + its embedding dimension.
/// Unknown names are a config error, not a panic.
fn resolve_model(name: &str) -> Result<(EmbeddingModel, usize), EmbedError> {
    match name {
        "bge-small-en-v1.5" | "BGESmallENV15" => Ok((EmbeddingModel::BGESmallENV15, 384)),
        "all-MiniLM-L6-v2" | "AllMiniLML6V2" => Ok((EmbeddingModel::AllMiniLML6V2, 384)),
        other => Err(EmbedError::Init(format!(
            "unknown embedding model: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_is_init_error() {
        let result = FastembedEmbedder::new("does-not-exist", 1);
        assert!(matches!(result, Err(EmbedError::Init(_))));
    }

    // Downloads the model on first run + needs the ONNX runtime; excluded from
    // default `cargo test`. Run with: `cargo test -p apollo-schema-search -- --ignored`
    #[test]
    #[ignore = "downloads the bge-small model + ONNX runtime; run explicitly with --ignored"]
    fn real_model_embeds_and_ranks_semantically() {
        let e = FastembedEmbedder::new("bge-small-en-v1.5", 1).expect("model init");
        assert_eq!(e.dimensions(), 384);
        let v = e
            .embed(&[
                "send a message to a channel".to_string(), // 0
                "post a chat message".to_string(),         // 1: close to 0
                "delete a database backup".to_string(),    // 2: far from 0
            ])
            .expect("embed");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].len(), 384);
        // cosine(0,1) should exceed cosine(0,2) — semantically closer.
        let cos = |a: &[f32], b: &[f32]| {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            dot / (na * nb)
        };
        assert!(
            cos(&v[0], &v[1]) > cos(&v[0], &v[2]),
            "expected 'send message' closer to 'post chat message' than to 'delete backup'"
        );
    }
}
