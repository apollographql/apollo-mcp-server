//! Postgres-backed [`EmbeddingStore`]: a shared, multi-writer cache. Rows are
//! generation-keyed by the exact `(model_id, dim, dtype, doc_builder_ver)` tuple
//! plus the content hash, so different embedding generations coexist without
//! deletion or contention — the invalidation model that a single shared database
//! needs, unlike the single-writer SQLite file.

use crate::embedding_store::{CacheError, EmbeddingStore, VECTOR_DTYPE, blob_to_vec, vec_to_blob};
use postgres::{Client, NoTls};

/// Map a `postgres::Error` into the shared `CacheError`.
fn backend(e: postgres::Error) -> CacheError {
    CacheError::Backend(e.to_string())
}

/// Shared, multi-writer embedding cache. Rows are keyed by the exact embedding
/// generation tuple plus the content hash, so different generations coexist
/// without deletion or contention.
pub struct PostgresCache {
    client: Client,
    model_id: String,
    dim: i32,
    dtype: String,
    doc_builder_ver: i64,
}

impl PostgresCache {
    /// Connect and ensure the schema exists. `url` is a libpq/URL connection
    /// string (e.g. `host=... user=... password=... dbname=...` or
    /// `postgres://user:pw@host/db`). Plaintext (`NoTls`) — in-cluster only.
    pub fn open(
        url: &str,
        model_id: &str,
        dim: usize,
        doc_builder_ver: i64,
    ) -> Result<Self, CacheError> {
        let mut client = Client::connect(url, NoTls).map_err(backend)?;
        // Idempotent: safe for concurrent replicas to run this at once.
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS embeddings (
                     model_id        text    NOT NULL,
                     dim             integer NOT NULL,
                     dtype           text    NOT NULL,
                     doc_builder_ver bigint  NOT NULL,
                     op_key          text    NOT NULL,
                     vector          bytea   NOT NULL,
                     PRIMARY KEY (model_id, dim, dtype, doc_builder_ver, op_key)
                 );",
            )
            .map_err(backend)?;
        Ok(Self {
            client,
            model_id: model_id.to_string(),
            dim: dim as i32,
            dtype: VECTOR_DTYPE.to_string(),
            doc_builder_ver,
        })
    }
}

impl EmbeddingStore for PostgresCache {
    fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, CacheError> {
        let row = self
            .client
            .query_opt(
                "SELECT vector FROM embeddings
                 WHERE model_id = $1 AND dim = $2 AND dtype = $3
                   AND doc_builder_ver = $4 AND op_key = $5",
                &[
                    &self.model_id,
                    &self.dim,
                    &self.dtype,
                    &self.doc_builder_ver,
                    &key,
                ],
            )
            .map_err(backend)?;
        match row {
            Some(r) => {
                let bytes: Vec<u8> = r.try_get(0).map_err(backend)?;
                let dim = usize::try_from(self.dim)
                    .map_err(|_| CacheError::Backend("negative dim".into()))?;
                Ok(Some(blob_to_vec(key, &bytes, dim)?))
            }
            None => Ok(None),
        }
    }

    fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError> {
        let dim =
            usize::try_from(self.dim).map_err(|_| CacheError::Backend("negative dim".into()))?;
        let mut tx = self.client.transaction().map_err(backend)?;
        let stmt = tx
            .prepare(
                "INSERT INTO embeddings
                   (model_id, dim, dtype, doc_builder_ver, op_key, vector)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (model_id, dim, dtype, doc_builder_ver, op_key) DO NOTHING",
            )
            .map_err(backend)?;
        for (key, vector) in entries {
            if vector.len() != dim {
                // Drop the tx uncommitted → rollback; no partial write.
                return Err(CacheError::BadVectorLen {
                    key: key.clone(),
                    expected: dim * 4,
                    got: vector.len() * 4,
                });
            }
            let blob = vec_to_blob(vector);
            tx.execute(
                &stmt,
                &[
                    &self.model_id,
                    &self.dim,
                    &self.dtype,
                    &self.doc_builder_ver,
                    key,
                    &blob,
                ],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration tests require a live Postgres. Run:
    ///   docker run --rm -e POSTGRES_PASSWORD=pw -p 5432:5432 postgres:16
    ///   EMBEDDING_CACHE_DATABASE_URL="host=localhost user=postgres password=pw dbname=postgres" \
    ///     cargo test -p apollo-schema-search -- --ignored
    fn url() -> Option<String> {
        std::env::var("EMBEDDING_CACHE_DATABASE_URL").ok()
    }

    #[test]
    #[ignore = "requires EMBEDDING_CACHE_DATABASE_URL"]
    fn roundtrip_put_then_get() {
        let Some(u) = url() else { return };
        let mut c = PostgresCache::open(&u, "m-roundtrip", 3, 1).unwrap();
        c.put_batch(&[("k1".to_string(), vec![1.0, -2.5, 0.0])])
            .unwrap();
        assert_eq!(c.get("k1").unwrap(), Some(vec![1.0, -2.5, 0.0]));
        assert_eq!(c.get("missing").unwrap(), None);
    }

    #[test]
    #[ignore = "requires EMBEDDING_CACHE_DATABASE_URL"]
    fn generations_are_isolated_without_deletion() {
        let Some(u) = url() else { return };
        // model-a writes a row.
        let mut a = PostgresCache::open(&u, "m-iso-a", 2, 1).unwrap();
        a.put_batch(&[("shared".to_string(), vec![0.1, 0.2])])
            .unwrap();
        // model-b (same dim) cannot see model-a's row...
        let mut b = PostgresCache::open(&u, "m-iso-b", 2, 1).unwrap();
        assert_eq!(b.get("shared").unwrap(), None);
        // ...and opening model-b did NOT delete model-a's row.
        let mut a2 = PostgresCache::open(&u, "m-iso-a", 2, 1).unwrap();
        assert_eq!(a2.get("shared").unwrap(), Some(vec![0.1, 0.2]));
    }

    #[test]
    #[ignore = "requires EMBEDDING_CACHE_DATABASE_URL"]
    fn put_batch_rejects_wrong_length() {
        let Some(u) = url() else { return };
        let mut c = PostgresCache::open(&u, "m-badlen", 3, 1).unwrap();
        let r = c.put_batch(&[("k".to_string(), vec![1.0f32, 2.0])]);
        assert!(matches!(r, Err(CacheError::BadVectorLen { .. })));
    }

    #[test]
    #[ignore = "requires EMBEDDING_CACHE_DATABASE_URL"]
    fn concurrent_put_is_idempotent() {
        let Some(u) = url() else { return };
        let mut a = PostgresCache::open(&u, "m-conc", 2, 1).unwrap();
        let mut b = PostgresCache::open(&u, "m-conc", 2, 1).unwrap();
        let entry = vec![("dup".to_string(), vec![0.3, 0.4])];
        a.put_batch(&entry).unwrap();
        // Second writer, same key/generation: ON CONFLICT DO NOTHING → no error.
        b.put_batch(&entry).unwrap();
        assert_eq!(a.get("dup").unwrap(), Some(vec![0.3, 0.4]));
    }
}
