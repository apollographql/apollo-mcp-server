//! Storage contract for operation embeddings plus the shared serialization used
//! by every backend. Vectors are content-addressed by SHA-256 of each
//! operation's document text; each backend decides how it persists them.

use sha2::{Digest, Sha256};

/// Bump when `enumerate_operation_documents` changes the document text it
/// produces, so a cache built with a different doc shape is discarded.
pub const DOC_BUILDER_VERSION: i64 = 1;

/// Byte format tag: raw little-endian f32.
pub(crate) const VECTOR_DTYPE: &str = "f32le";

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache backend error: {0}")]
    Backend(String),
    #[error("cached vector for {key} has {got} bytes, expected {expected}")]
    BadVectorLen {
        key: String,
        expected: usize,
        got: usize,
    },
}

/// A backend that persists operation embeddings keyed by content hash.
///
/// `get` returns `Ok(None)` on a miss. Both methods take `&mut self` because the
/// synchronous Postgres client requires it; SQLite tolerates it.
pub trait EmbeddingStore: Send {
    fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, CacheError>;
    fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError>;
}

/// SHA-256 hex of an operation document's text — the cache key.
pub fn doc_key(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Serialize a vector to raw little-endian f32 bytes.
pub(crate) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for &x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    buf
}

/// Deserialize raw little-endian f32 bytes, validating the length matches `dim`.
pub(crate) fn blob_to_vec(key: &str, bytes: &[u8], dim: usize) -> Result<Vec<f32>, CacheError> {
    let expected = dim * 4;
    if bytes.len() != expected {
        return Err(CacheError::BadVectorLen {
            key: key.to_string(),
            expected,
            got: bytes.len(),
        });
    }
    let mut out = Vec::with_capacity(dim);
    for chunk in bytes.chunks_exact(4) {
        let arr: [u8; 4] = chunk.try_into().map_err(|_| CacheError::BadVectorLen {
            key: key.to_string(),
            expected,
            got: bytes.len(),
        })?;
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_key_is_stable_and_content_addressed() {
        assert_eq!(doc_key("send message"), doc_key("send message"));
        assert_ne!(doc_key("send message"), doc_key("send messages"));
    }

    #[test]
    fn blob_roundtrip_and_length_guard() {
        let v = vec![1.0f32, -2.5, 0.0];
        let blob = vec_to_blob(&v);
        assert_eq!(blob_to_vec("k", &blob, 3).unwrap(), v);
        assert!(matches!(
            blob_to_vec("k", &blob, 4),
            Err(CacheError::BadVectorLen { .. })
        ));
    }
}
