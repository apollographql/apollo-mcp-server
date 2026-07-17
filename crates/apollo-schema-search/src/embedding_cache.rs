//! On-disk cache of operation embeddings so the corpus is embedded once and
//! reloaded on later starts. Vectors are content-addressed by SHA-256 of each
//! operation's document text: a changed operation misses and is re-embedded, an
//! unchanged one is reused. A one-row `meta` guard wipes the vectors when the
//! embedding *semantics* change (model, dimensionality, byte format, doc format).

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Bump when `enumerate_operation_documents` changes the document text it
/// produces, so a cache built with a different doc shape is discarded wholesale.
pub const DOC_BUILDER_VERSION: i64 = 1;

/// Byte format tag stored in `meta.dtype`: raw little-endian f32.
const VECTOR_DTYPE: &str = "f32le";

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("cached vector for {key} has {got} bytes, expected {expected}")]
    BadVectorLen {
        key: String,
        expected: usize,
        got: usize,
    },
}

/// SHA-256 hex of an operation document's text — the cache key.
pub fn doc_key(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Serialize a vector to raw little-endian f32 bytes.
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for &x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    buf
}

/// Deserialize raw little-endian f32 bytes, validating the length matches `dim`.
fn blob_to_vec(key: &str, bytes: &[u8], dim: usize) -> Result<Vec<f32>, CacheError> {
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

pub struct EmbeddingCache {
    conn: Connection,
    dim: usize,
}

impl EmbeddingCache {
    /// Open (or create) the cache at `path`, validating the `meta` guard against
    /// the current `(model_id, dim, dtype, doc_builder_ver)`. On any mismatch (or a
    /// fresh file) the stored vectors are cleared and the new meta written, so they
    /// re-embed with the current settings.
    pub fn open(
        path: &Path,
        model_id: &str,
        dim: usize,
        doc_builder_ver: i64,
    ) -> Result<Self, CacheError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 id INTEGER PRIMARY KEY CHECK (id = 0),
                 model_id TEXT NOT NULL,
                 dim INTEGER NOT NULL,
                 dtype TEXT NOT NULL,
                 doc_builder_ver INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS embeddings (
                 op_key TEXT PRIMARY KEY,
                 vector BLOB NOT NULL
             ) WITHOUT ROWID;",
        )?;
        let current = (
            model_id.to_string(),
            dim as i64,
            VECTOR_DTYPE.to_string(),
            doc_builder_ver,
        );
        let existing: Option<(String, i64, String, i64)> = conn
            .query_row(
                "SELECT model_id, dim, dtype, doc_builder_ver FROM meta WHERE id = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if existing.as_ref() != Some(&current) {
            conn.execute("DELETE FROM embeddings", [])?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (id, model_id, dim, dtype, doc_builder_ver)
                 VALUES (0, ?1, ?2, ?3, ?4)",
                params![current.0, current.1, current.2, current.3],
            )?;
        }
        Ok(Self { conn, dim })
    }

    /// Fetch a cached vector by content key. `Ok(None)` if absent; `Err` if the
    /// stored bytes are the wrong length for `dim` (caller treats that as a miss).
    pub fn get(&self, key: &str) -> Result<Option<Vec<f32>>, CacheError> {
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT vector FROM embeddings WHERE op_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        match blob {
            Some(bytes) => Ok(Some(blob_to_vec(key, &bytes, self.dim)?)),
            None => Ok(None),
        }
    }

    /// Insert/replace `(key, vector)` pairs in a single transaction.
    ///
    /// Validates every entry's vector length against `dim` up front, before opening
    /// the transaction, so a bad entry is rejected without writing anything partial.
    pub fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError> {
        for (key, vector) in entries {
            if vector.len() != self.dim {
                return Err(CacheError::BadVectorLen {
                    key: key.clone(),
                    expected: self.dim * 4,
                    got: vector.len() * 4,
                });
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("INSERT OR REPLACE INTO embeddings (op_key, vector) VALUES (?1, ?2)")?;
            for (key, vector) in entries {
                stmt.execute(params![key, vec_to_blob(vector)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Unique temp DB path (no external temp-dir crate); removed if it already exists.
    fn temp_db(tag: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("emb_cache_{}_{}_{}.db", tag, std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn roundtrip_put_then_get() {
        let path = temp_db("roundtrip");
        let mut cache = EmbeddingCache::open(&path, "m", 3, 1).unwrap();
        cache
            .put_batch(&[("k1".to_string(), vec![1.0, -2.5, 0.0])])
            .unwrap();
        let got = cache.get("k1").unwrap();
        assert_eq!(got, Some(vec![1.0, -2.5, 0.0]));
        assert_eq!(cache.get("missing").unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn meta_mismatch_wipes_vectors() {
        let path = temp_db("meta");
        {
            let mut cache = EmbeddingCache::open(&path, "model-a", 3, 1).unwrap();
            cache
                .put_batch(&[("k1".to_string(), vec![1.0, 2.0, 3.0])])
                .unwrap();
            assert!(cache.get("k1").unwrap().is_some());
        }
        // Reopen with a different model id -> vectors must be cleared.
        let cache = EmbeddingCache::open(&path, "model-b", 3, 1).unwrap();
        assert_eq!(cache.get("k1").unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn meta_match_preserves_vectors_across_reopen() {
        let path = temp_db("persist");
        {
            let mut cache = EmbeddingCache::open(&path, "m", 2, 1).unwrap();
            cache
                .put_batch(&[("k".to_string(), vec![0.5, 0.5])])
                .unwrap();
        }
        let cache = EmbeddingCache::open(&path, "m", 2, 1).unwrap();
        assert_eq!(cache.get("k").unwrap(), Some(vec![0.5, 0.5]));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn put_batch_rejects_wrong_length() {
        // A 2-float vector under a dim=3 cache must be rejected up front by put_batch,
        // not silently stored and only caught later on get().
        let path = temp_db("badlen");
        let mut c = EmbeddingCache::open(&path, "m", 3, 1).unwrap();
        let result = c.put_batch(&[("k".to_string(), vec![1.0f32, 2.0])]);
        assert!(matches!(result, Err(CacheError::BadVectorLen { .. })));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn doc_key_is_stable_and_content_addressed() {
        assert_eq!(doc_key("send message"), doc_key("send message"));
        assert_ne!(doc_key("send message"), doc_key("send messages"));
    }
}
