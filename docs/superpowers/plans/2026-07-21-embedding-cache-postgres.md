# Embedding Cache — Postgres Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Amendment (2026-07-21, during implementation):** SQLite support was removed.
> `PostgresCache` is the only backend — `SqliteCache`, the `rusqlite` dependency,
> and the tagged `CacheConfig` enum are gone. Config is a single field
> `semantic.cache_url: Option<String>`, threaded as `Option<String>` (not
> `Option<CacheConfig>`) through the plumbing; `open_store` is Postgres-only. The
> `EmbeddingStore` trait is kept for the in-memory test double. Task/step details
> below that mention SQLite, `rusqlite`, or the enum are superseded accordingly.

**Goal:** Externalize the semantic-search embedding cache behind an `EmbeddingStore` trait and add a Postgres backend (alongside the existing SQLite one), selectable via config, so all MCP replicas share one durable cache.

**Architecture:** Extract today's concrete `EmbeddingCache` into an `EmbeddingStore` trait with two implementations — `SqliteCache` (renamed, unchanged behavior) and `PostgresCache` (new, generation-keyed for safe multi-writer sharing). `VectorSearch::build` consumes `Option<&mut dyn EmbeddingStore>`. A tagged-enum `cache` config selects the backend and threads through the existing server plumbing. Fail-open behavior is preserved end to end. In-memory brute-force search is unchanged.

**Tech Stack:** Rust, `rusqlite` (bundled), `postgres` 0.19 (sync client, `NoTls`), `sha2`, `serde`/`schemars`, `rstest`, `insta`.

**Scope:** This plan covers the `apollo-mcp-server` code only. The Helm/deploy work (the `embedding-db` StatefulSet in `constellation-runtime`) is a **separate follow-up plan** in that repo; the contract it depends on is the config surface (Task 4) and the env var `EMBEDDING_CACHE_DATABASE_URL`.

## Global Constraints

- Clippy `--deny warnings`; these are hard-denied in library code: `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `exit`. Use `?`, `match`, `if let`, `.get()`. (Test modules may use `.unwrap()` per the existing convention in this crate.)
- `cargo fmt` clean; `cargo clippy --all-targets -- --deny warnings` clean.
- New/modified code needs ≥80% patch coverage.
- Vector serialization is fixed: raw little-endian f32, `dim×4` bytes, `dtype = "f32le"`.
- Content key is unchanged: `op_key = hex(SHA-256(OperationDocument.text))` (full SHA-256).
- `DOC_BUILDER_VERSION` const stays `1`; bump only when `enumerate_operation_documents` changes doc text format.
- Postgres backend must be **generation-keyed by the exact tuple** `(model_id, dim, dtype, doc_builder_ver, op_key)` as PRIMARY KEY — no hashing on the generation axis, no destructive `DELETE` on open.
- Fail-open everywhere: open/connect failure → no cache (embed from scratch, warn); `get` error → miss; `put_batch` error → log + continue. The server always comes up.

---

## File Structure

**`crates/apollo-schema-search/`**
- Create `src/embedding_store.rs` — the `EmbeddingStore` trait, `CacheError`, `DOC_BUILDER_VERSION`, `VECTOR_DTYPE`, and shared free fns `doc_key`, `vec_to_blob`, `blob_to_vec`. One responsibility: the storage contract + shared serialization.
- Rename `src/embedding_cache.rs` → `src/sqlite_cache.rs` — `SqliteCache` struct implementing `EmbeddingStore`. Keeps its one-row `meta` guard (single-writer file semantics).
- Create `src/postgres_cache.rs` — `PostgresCache` struct implementing `EmbeddingStore`, generation-keyed.
- Modify `src/lib.rs` — module declarations and re-exports.
- Modify `src/vector_search.rs` — `build` takes `Option<&mut dyn EmbeddingStore>`.
- Modify `Cargo.toml` — add `postgres = "0.19"`.

**`crates/apollo-mcp-server/`**
- Modify `src/runtime/introspection.rs` — replace `SemanticConfig.cache_path` with `cache: Option<CacheConfig>`; add the `CacheConfig` enum.
- Modify `src/introspection/tools/search.rs` — construct the store from `CacheConfig`; store `cache_config` on `Search`.
- Modify `src/main.rs`, `src/server.rs`, `src/server/states.rs`, `src/server/states/starting.rs` — thread `Option<CacheConfig>` in place of `Option<PathBuf>`.
- Modify `.changeset/embedding_cache.md` — describe the Postgres backend.

---

## Task 1: Extract the `EmbeddingStore` trait; rename SQLite backend

**Files:**
- Create: `crates/apollo-schema-search/src/embedding_store.rs`
- Rename + modify: `crates/apollo-schema-search/src/embedding_cache.rs` → `crates/apollo-schema-search/src/sqlite_cache.rs`
- Modify: `crates/apollo-schema-search/src/lib.rs`
- Modify: `crates/apollo-schema-search/src/vector_search.rs`
- Modify: `crates/apollo-mcp-server/src/introspection/tools/search.rs`

**Interfaces:**
- Produces:
  - `trait EmbeddingStore: Send { fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, CacheError>; fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError>; }`
  - `enum CacheError { Sqlite(rusqlite::Error), BadVectorLen { key, expected, got }, Backend(String) }`
  - `fn doc_key(text: &str) -> String`, `pub(crate) fn vec_to_blob(&[f32]) -> Vec<u8>`, `pub(crate) fn blob_to_vec(key, &[u8], dim) -> Result<Vec<f32>, CacheError>`
  - `const DOC_BUILDER_VERSION: i64 = 1`
  - `struct SqliteCache` with `pub fn open(path: &Path, model_id: &str, dim: usize, doc_builder_ver: i64) -> Result<Self, CacheError>` implementing `EmbeddingStore`
  - `VectorSearch::build(schema, root_types, flatten_depth, embedder, cache: Option<&mut dyn EmbeddingStore>)`

- [ ] **Step 1: Write the failing test** (a build using a trait-object store double)

Add to the bottom of `crates/apollo-schema-search/src/vector_search.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
    /// Minimal in-memory `EmbeddingStore` double: exercises the trait-object
    /// build path without SQLite or Postgres.
    struct MemoryStore {
        map: std::collections::HashMap<String, Vec<f32>>,
    }
    impl MemoryStore {
        fn new() -> Self {
            Self { map: std::collections::HashMap::new() }
        }
    }
    impl crate::EmbeddingStore for MemoryStore {
        fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, crate::CacheError> {
            Ok(self.map.get(key).cloned())
        }
        fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), crate::CacheError> {
            for (k, v) in entries {
                self.map.insert(k.clone(), v.clone());
            }
            Ok(())
        }
    }

    #[test]
    fn build_reuses_via_trait_object_store() {
        let schema = Schema::parse(SCHEMA, "s.graphql").unwrap().validate().unwrap();
        let roots = OperationType::Query | OperationType::Mutation;
        let mut store = MemoryStore::new();

        // First build populates the store.
        VectorSearch::build(&schema, roots, 2, Arc::new(FakeEmbedder::new(64)), Some(&mut store as &mut dyn crate::EmbeddingStore)).unwrap();
        assert!(!store.map.is_empty(), "first build should populate the store");

        // Second build: an embedder that panics if called proves all-cache-hit.
        struct NoEmbed;
        impl Embedder for NoEmbed {
            fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, crate::EmbedError> {
                Err(crate::EmbedError::Inference("should not embed".into()))
            }
            fn dimensions(&self) -> usize { 64 }
        }
        let vs = VectorSearch::build(&schema, roots, 2, Arc::new(NoEmbed), Some(&mut store as &mut dyn crate::EmbeddingStore)).unwrap();
        assert!(!vs.search("user by email", None, 10).unwrap().is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-schema-search build_reuses_via_trait_object_store`
Expected: FAIL to compile — `EmbeddingStore` and the `Option<&mut dyn EmbeddingStore>` signature don't exist yet.

- [ ] **Step 3: Create `embedding_store.rs`**

Create `crates/apollo-schema-search/src/embedding_store.rs`:

```rust
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
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
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
```

- [ ] **Step 4: Rename `embedding_cache.rs` → `sqlite_cache.rs` and reduce it to `SqliteCache`**

Run: `git mv crates/apollo-schema-search/src/embedding_cache.rs crates/apollo-schema-search/src/sqlite_cache.rs`

Then replace the top of the file (the error enum, consts, and free fns now live in `embedding_store.rs`) so it reads:

```rust
//! SQLite-backed [`EmbeddingStore`]: a single-file, single-writer cache. A
//! one-row `meta` guard wipes the vectors when the embedding semantics change
//! (model, dimensionality, byte format, doc format).

use crate::embedding_store::{CacheError, EmbeddingStore, VECTOR_DTYPE, blob_to_vec, vec_to_blob};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct SqliteCache {
    conn: Connection,
    dim: usize,
}

impl SqliteCache {
    /// Open (or create) the cache at `path`, validating the `meta` guard against
    /// the current `(model_id, dim, dtype, doc_builder_ver)`. On any mismatch (or a
    /// fresh file) the stored vectors are cleared and the new meta written.
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
}

impl EmbeddingStore for SqliteCache {
    fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, CacheError> {
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

    fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("INSERT OR REPLACE INTO embeddings (op_key, vector) VALUES (?1, ?2)")?;
            for (key, vector) in entries {
                if vector.len() != self.dim {
                    return Err(CacheError::BadVectorLen {
                        key: key.clone(),
                        expected: self.dim * 4,
                        got: vector.len() * 4,
                    });
                }
                stmt.execute(params![key, vec_to_blob(vector)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
```

Then, in the existing `#[cfg(test)] mod tests` at the bottom of `sqlite_cache.rs`: (a) replace every `EmbeddingCache::open` with `SqliteCache::open`; (b) delete the `doc_key_is_stable_and_content_addressed` test (it now lives in `embedding_store.rs`); (c) change the `use super::*;` to also pull the error type: add `use crate::embedding_store::CacheError;` if the `put_batch_rejects_wrong_length` test references `CacheError` (it does). Leave the other four tests (`roundtrip_put_then_get`, `meta_mismatch_wipes_vectors`, `meta_match_preserves_vectors_across_reopen`, `put_batch_rejects_wrong_length`) intact aside from the rename.

- [ ] **Step 5: Update `lib.rs`**

Replace `crates/apollo-schema-search/src/lib.rs` module + re-export lines so the new layout is exposed:

```rust
//! Hybrid (lexical + semantic) search over GraphQL schema operations.
mod embedder;
mod embedding_store;
mod fastembed_embedder;
mod fusion;
mod hybrid;
mod postgres_cache;
mod sqlite_cache;
mod vector_search;
mod vector_store;

pub use embedder::{EmbedError, Embedder, FakeEmbedder};
pub use embedding_store::{CacheError, DOC_BUILDER_VERSION, EmbeddingStore, doc_key};
pub use fastembed_embedder::FastembedEmbedder;
pub use fusion::rrf_fuse;
pub use hybrid::HybridSearch;
pub use postgres_cache::PostgresCache;
pub use sqlite_cache::SqliteCache;
pub use vector_search::VectorSearch;
pub use vector_store::{InMemoryVectorStore, VectorStore};
```

Note: `postgres_cache` / `PostgresCache` are referenced here but created in Task 3. To keep this task compiling on its own, create a placeholder now: `crates/apollo-schema-search/src/postgres_cache.rs` containing a single line comment `// PostgresCache implemented in Task 3.` and TEMPORARILY drop the `mod postgres_cache;` + `pub use postgres_cache::PostgresCache;` lines from the block above (re-add them in Task 3). Simpler: omit both postgres lines in this task, add them in Task 3.

- [ ] **Step 6: Change `VectorSearch::build` signature and internals**

In `crates/apollo-schema-search/src/vector_search.rs`:

Change the imports at the top from `use crate::embedding_cache::{EmbeddingCache, doc_key};` to:

```rust
use crate::embedding_store::{EmbeddingStore, doc_key};
```

Change the `build` signature parameter from `cache: Option<&mut EmbeddingCache>` to (note the `mut` binding — `as_deref_mut()` needs it):

```rust
        mut cache: Option<&mut dyn EmbeddingStore>,
```

The body already only calls `cache.as_deref()` for `get` and `if let Some(c) = cache` for `put_batch`. Because `get` is now `&mut self`, change the pass-1 lookup from:

```rust
            let hit = match cache.as_deref() {
                Some(c) => c.get(&key).unwrap_or(None),
                None => None,
            };
```
to:
```rust
            let hit = match cache.as_deref_mut() {
                Some(c) => c.get(&key).unwrap_or(None),
                None => None,
            };
```

Leave the rest of `build` unchanged. In the existing `second_build_reuses_cache_and_embeds_nothing` test, replace `EmbeddingCache::open` with `SqliteCache::open` and add `use crate::SqliteCache;` (or `crate::sqlite_cache::SqliteCache`) to that test module's imports.

- [ ] **Step 7: Update the MCP call site**

In `crates/apollo-mcp-server/src/introspection/tools/search.rs`, the import line currently reads:

```rust
use apollo_schema_search::{
    DOC_BUILDER_VERSION, Embedder, EmbeddingCache, FastembedEmbedder, HybridSearch, VectorSearch,
};
```
Change `EmbeddingCache` → `SqliteCache`:
```rust
use apollo_schema_search::{
    DOC_BUILDER_VERSION, Embedder, FastembedEmbedder, HybridSearch, SqliteCache, VectorSearch,
};
```
Also add `EmbeddingStore` to that import list (needed for the explicit cast below):
```rust
use apollo_schema_search::{
    DOC_BUILDER_VERSION, Embedder, EmbeddingStore, FastembedEmbedder, HybridSearch, SqliteCache,
    VectorSearch,
};
```
In `build_backend`, replace the `EmbeddingCache::open(...)` call with `SqliteCache::open(...)` (the surrounding `cache_path.and_then(...)` logic is unchanged for now — it is replaced wholesale in Task 5). Then change the build call to cast explicitly, because `Option<&mut SqliteCache>` does **not** auto-coerce to `Option<&mut dyn EmbeddingStore>` through `Option`:
```rust
            match VectorSearch::build(
                schema,
                root_types,
                flatten_depth,
                emb,
                cache.as_mut().map(|c| c as &mut dyn EmbeddingStore),
            ) {
```

- [ ] **Step 8: Run the whole crate + the MCP crate build**

Run: `cargo test -p apollo-schema-search && cargo build -p apollo-mcp-server`
Expected: PASS — all existing SQLite/vector tests green, plus `build_reuses_via_trait_object_store`, plus `blob_roundtrip_and_length_guard`.

- [ ] **Step 9: Lint + format**

Run: `cargo fmt && cargo clippy -p apollo-schema-search --all-targets -- --deny warnings`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add crates/apollo-schema-search/src crates/apollo-mcp-server/src/introspection/tools/search.rs
git commit -m "refactor(air-311): extract EmbeddingStore trait; rename SqliteCache

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Add the `postgres` dependency

**Files:**
- Modify: `crates/apollo-schema-search/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `crates/apollo-schema-search/Cargo.toml`, under `[dependencies]` (keep alphabetical with the existing entries), add:

```toml
postgres = "0.19"
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo build -p apollo-schema-search`
Expected: PASS (pulls `postgres` + `tokio-postgres`; pure Rust, no system libs with `NoTls`).

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-schema-search/Cargo.toml Cargo.lock
git commit -m "build(air-311): add postgres crate for the embedding cache

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Implement `PostgresCache` (generation-keyed, multi-writer-safe)

**Files:**
- Create: `crates/apollo-schema-search/src/postgres_cache.rs`
- Modify: `crates/apollo-schema-search/src/lib.rs` (re-add the postgres module + export deferred in Task 1)

**Interfaces:**
- Produces: `struct PostgresCache` with `pub fn open(url: &str, model_id: &str, dim: usize, doc_builder_ver: i64) -> Result<Self, CacheError>` implementing `EmbeddingStore`.

- [ ] **Step 1: Write the failing integration test**

Create `crates/apollo-schema-search/src/postgres_cache.rs` with ONLY the test module first (implementation follows in Step 3). These tests are `#[ignore]`d — they need a live Postgres and run via `EMBEDDING_CACHE_DATABASE_URL`:

```rust
//! Postgres-backed [`EmbeddingStore`]: a shared, multi-writer cache. Rows are
//! generation-keyed by the exact `(model_id, dim, dtype, doc_builder_ver)` tuple
//! so different embedding generations never collide and never delete each other.

// implementation added in Step 3

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmbeddingStore;

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
        c.put_batch(&[("k1".to_string(), vec![1.0, -2.5, 0.0])]).unwrap();
        assert_eq!(c.get("k1").unwrap(), Some(vec![1.0, -2.5, 0.0]));
        assert_eq!(c.get("missing").unwrap(), None);
    }

    #[test]
    #[ignore = "requires EMBEDDING_CACHE_DATABASE_URL"]
    fn generations_are_isolated_without_deletion() {
        let Some(u) = url() else { return };
        // model-a writes a row.
        let mut a = PostgresCache::open(&u, "m-iso-a", 2, 1).unwrap();
        a.put_batch(&[("shared".to_string(), vec![0.1, 0.2])]).unwrap();
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
        assert!(matches!(r, Err(crate::CacheError::BadVectorLen { .. })));
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p apollo-schema-search --no-run`
Expected: FAIL to compile — `PostgresCache` doesn't exist yet.

- [ ] **Step 3: Write the implementation**

At the top of `crates/apollo-schema-search/src/postgres_cache.rs` (above the test module), add:

```rust
use crate::embedding_store::{
    CacheError, EmbeddingStore, VECTOR_DTYPE, blob_to_vec, vec_to_blob,
};
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
    /// Connect and ensure the schema exists. The connection string is
    /// libpq/URL form (e.g. `host=... user=... password=... dbname=...` or
    /// `postgres://user:pw@host/db`). Plaintext (`NoTls`) — in-cluster only.
    pub fn open(
        url: &str,
        model_id: &str,
        dim: usize,
        doc_builder_ver: i64,
    ) -> Result<Self, CacheError> {
        let mut client = Client::connect(url, NoTls).map_err(backend)?;
        // Idempotent: safe for concurrent replicas to run at once.
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
        let dim = usize::try_from(self.dim)
            .map_err(|_| CacheError::Backend("negative dim".into()))?;
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
```

- [ ] **Step 4: Re-add the module + export in `lib.rs`**

Add back the two lines deferred in Task 1: `mod postgres_cache;` (in the module list) and `pub use postgres_cache::PostgresCache;` (in the re-exports). Delete the placeholder comment if one was left.

- [ ] **Step 5: Compile-check (tests are ignored without a DB)**

Run: `cargo test -p apollo-schema-search`
Expected: PASS — the four Postgres tests are reported as `ignored`; everything else passes.

- [ ] **Step 6: (Optional, if Docker available) run the integration tests**

Run:
```bash
docker run --rm -d -e POSTGRES_PASSWORD=pw -p 5432:5432 --name emb-pg postgres:16
sleep 3
EMBEDDING_CACHE_DATABASE_URL="host=localhost user=postgres password=pw dbname=postgres" \
  cargo test -p apollo-schema-search -- --ignored
docker rm -f emb-pg
```
Expected: the four `PostgresCache` tests PASS.

- [ ] **Step 7: Lint + format + commit**

```bash
cargo fmt && cargo clippy -p apollo-schema-search --all-targets -- --deny warnings
git add crates/apollo-schema-search/src/postgres_cache.rs crates/apollo-schema-search/src/lib.rs
git commit -m "feat(air-311): generation-keyed Postgres embedding cache

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: `CacheConfig` config surface

**Files:**
- Modify: `crates/apollo-mcp-server/src/runtime/introspection.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Debug, Deserialize, JsonSchema)]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum CacheConfig {
      Sqlite { path: std::path::PathBuf },
      Postgres { url: String },
  }
  ```
  and `SemanticConfig.cache: Option<CacheConfig>` (replacing `cache_path`).

- [ ] **Step 1: Write the failing tests**

In `crates/apollo-mcp-server/src/runtime/introspection.rs`, replace the existing `semantic_cache_path_parses` test with:

```rust
    #[test]
    fn semantic_cache_postgres_parses() {
        let c: SemanticConfig = serde_yaml::from_str(
            "enabled: true\nmodel: bge-small-en-v1.5\ninference_threads: 2\ncache:\n  type: postgres\n  url: postgres://u:p@db/emb\n",
        )
        .unwrap();
        assert!(matches!(
            c.cache,
            Some(CacheConfig::Postgres { ref url }) if url == "postgres://u:p@db/emb"
        ));
    }

    #[test]
    fn semantic_cache_sqlite_parses() {
        let c: SemanticConfig = serde_yaml::from_str(
            "cache:\n  type: sqlite\n  path: /data/emb.db\n",
        )
        .unwrap();
        assert!(matches!(
            c.cache,
            Some(CacheConfig::Sqlite { ref path }) if path == std::path::Path::new("/data/emb.db")
        ));
    }

    #[test]
    fn semantic_cache_defaults_to_none() {
        let c = SemanticConfig::default();
        assert!(c.cache.is_none());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p apollo-mcp-server semantic_cache`
Expected: FAIL to compile — `CacheConfig` and `SemanticConfig.cache` don't exist.

- [ ] **Step 3: Add `CacheConfig` and swap the field**

In `crates/apollo-mcp-server/src/runtime/introspection.rs`, add the enum (place it just above `SemanticConfig`):

```rust
/// Backend for the semantic-search embedding cache. Omit `cache` entirely to
/// disable caching (embed on every start).
///
/// Note: `deny_unknown_fields` is intentionally NOT set here — serde does not
/// support it on internally-tagged enums. The enclosing `SemanticConfig` still
/// rejects unknown keys.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CacheConfig {
    /// Single-file SQLite cache. Relative paths resolve against CWD.
    Sqlite { path: std::path::PathBuf },
    /// Shared Postgres cache. `url` is a libpq/URL connection string; source the
    /// password from an env var (e.g. `${env.EMBEDDING_CACHE_DATABASE_URL}`).
    Postgres { url: String },
}
```

In `SemanticConfig`, replace the `cache_path` field:

```rust
    /// Embedding cache backend. Unset = disabled (embed on every start).
    #[serde(default)]
    pub cache: Option<CacheConfig>,
```

And in `impl Default for SemanticConfig`, replace `cache_path: None,` with `cache: None,`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p apollo-mcp-server semantic_cache`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-mcp-server/src/runtime/introspection.rs
git commit -m "feat(air-311): tagged-enum cache config (sqlite|postgres)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: Thread `CacheConfig` through the server and build the store from it

**Files:**
- Modify: `crates/apollo-mcp-server/src/main.rs:203`
- Modify: `crates/apollo-mcp-server/src/server.rs:78,171,220`
- Modify: `crates/apollo-mcp-server/src/server/states.rs:73,137,540`
- Modify: `crates/apollo-mcp-server/src/server/states/starting.rs:140`
- Modify: `crates/apollo-mcp-server/src/introspection/tools/search.rs`

**Interfaces:**
- Consumes: `CacheConfig` (Task 4), `SqliteCache::open` / `PostgresCache::open` (Tasks 1, 3), `EmbeddingStore` (Task 1).
- Produces: `Search::new(..., semantic_cache: Option<CacheConfig>)` and an internal `open_store(cache: Option<&CacheConfig>, model_id: &str, dim: usize) -> Option<Box<dyn EmbeddingStore>>`.

- [ ] **Step 1: Write the failing test** (store selection from config)

In `crates/apollo-mcp-server/src/introspection/tools/search.rs`, add to the `#[cfg(test)] mod tests` (create the module if the file has none; otherwise append):

```rust
    #[test]
    fn open_store_none_when_no_config() {
        assert!(super::open_store(None, "m", 8).is_none());
    }

    #[test]
    fn open_store_sqlite_from_config() {
        use crate::runtime::introspection::CacheConfig;
        let dir = std::env::temp_dir().join(format!("emb_open_store_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let cfg = CacheConfig::Sqlite { path: dir.clone() };
        let store = super::open_store(Some(&cfg), "m", 4);
        assert!(store.is_some(), "sqlite store should open");
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn open_store_postgres_bad_url_is_fail_open_none() {
        use crate::runtime::introspection::CacheConfig;
        // Unreachable/garbage URL must fail open to None, never panic.
        let cfg = CacheConfig::Postgres { url: "host=127.0.0.1 port=1 user=nope dbname=nope connect_timeout=1".into() };
        assert!(super::open_store(Some(&cfg), "m", 4).is_none());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p apollo-mcp-server open_store`
Expected: FAIL to compile — `open_store` doesn't exist.

- [ ] **Step 3: Add `open_store` and rewrite `build_backend`'s cache handling**

In `crates/apollo-mcp-server/src/introspection/tools/search.rs`:

Update imports — add the config enum, the Postgres cache, and the trait:
```rust
use crate::runtime::introspection::CacheConfig;
use apollo_schema_search::{
    DOC_BUILDER_VERSION, Embedder, EmbeddingStore, FastembedEmbedder, HybridSearch, PostgresCache,
    SqliteCache, VectorSearch,
};
```

Add the factory near the top of the file (below `clamp_limit`):
```rust
/// Open the configured embedding store, or `None` when caching is disabled or the
/// backend fails to open (fail-open: the caller embeds from scratch).
fn open_store(
    cache: Option<&CacheConfig>,
    model_id: &str,
    dim: usize,
) -> Option<Box<dyn EmbeddingStore>> {
    match cache {
        None => None,
        Some(CacheConfig::Sqlite { path }) => {
            match SqliteCache::open(path, model_id, dim, DOC_BUILDER_VERSION) {
                Ok(c) => Some(Box::new(c)),
                Err(e) => {
                    warn!("embedding cache disabled (sqlite open failed at {path:?}): {e}");
                    None
                }
            }
        }
        Some(CacheConfig::Postgres { url }) => {
            match PostgresCache::open(url, model_id, dim, DOC_BUILDER_VERSION) {
                Ok(c) => Some(Box::new(c)),
                Err(e) => {
                    warn!("embedding cache disabled (postgres connect failed): {e}");
                    None
                }
            }
        }
    }
}
```

Change `build_backend`'s signature parameter `cache_path: Option<&Path>` → `cache: Option<&CacheConfig>`, and replace its cache-open block:
```rust
        Some(emb) => {
            let mut store = open_store(cache, model_id, emb.dimensions());
            match VectorSearch::build(schema, root_types, flatten_depth, emb, store.as_deref_mut()) {
                Ok(vector) => Ok(Box::new(HybridSearch::new(
                    vec![Box::new(index), Box::new(vector)],
                    rrf_k,
                ))),
                Err(e) => {
                    warn!("semantic index build failed; degrading to lexical-only: {e}");
                    Ok(Box::new(index))
                }
            }
        }
```
(`store.as_deref_mut()` yields `Option<&mut dyn EmbeddingStore>`.)

On the `Search` struct: replace the field `cache_path: Option<PathBuf>` with `cache_config: Option<CacheConfig>` (and its doc comment). Remove the now-unused `use std::path::{Path, PathBuf};` if nothing else needs them (keep `PathBuf` only if still referenced; after this task it is not — drop it).

In `Search::new`, change the last parameter from `semantic_cache_path: Option<PathBuf>` to `semantic_cache: Option<CacheConfig>`, and pass it through to `new_with_embedder` as `cache_config`.

In `new_with_embedder`, change the trailing `cache_path: Option<PathBuf>` parameter to `cache_config: Option<CacheConfig>`; pass `cache_config.as_ref()` into `build_backend`; store `cache_config` on the struct.

In `rebuild`, change `self.cache_path.as_deref()` to `self.cache_config.as_ref()` in the `build_backend` call.

- [ ] **Step 4: Update the plumbing chain (types only)**

Apply these type swaps (each is `Option<PathBuf>`/`Option<std::path::PathBuf>` → `Option<CacheConfig>`, and the field/param rename `semantic_cache_path` → `semantic_cache`). Add `use crate::runtime::introspection::CacheConfig;` where needed.

- `crates/apollo-mcp-server/src/server.rs`
  - line ~78: struct field `semantic_cache_path: Option<PathBuf>` → `semantic_cache: Option<CacheConfig>`
  - line ~171: builder param `#[builder(required)] semantic_cache_path: Option<PathBuf>` → `#[builder(required)] semantic_cache: Option<CacheConfig>`
  - line ~220: destructure `semantic_cache_path,` → `semantic_cache,`
- `crates/apollo-mcp-server/src/server/states.rs`
  - line ~73: field `semantic_cache_path: Option<std::path::PathBuf>` → `semantic_cache: Option<CacheConfig>`
  - line ~137: `semantic_cache_path: server.semantic_cache_path,` → `semantic_cache: server.semantic_cache,`
  - line ~540 (test defaults): `semantic_cache_path: None,` → `semantic_cache: None,`
- `crates/apollo-mcp-server/src/server/states/starting.rs`
  - line ~140: `self.config.semantic_cache_path.clone(),` → `self.config.semantic_cache.clone(),`
- `crates/apollo-mcp-server/src/main.rs`
  - line ~203: `.semantic_cache_path(config.introspection.search.semantic.cache_path.clone())` → `.semantic_cache(config.introspection.search.semantic.cache.clone())`

- [ ] **Step 5: Build + test the MCP crate**

Run: `cargo test -p apollo-mcp-server`
Expected: PASS — including the three `open_store` tests. The `open_store_postgres_bad_url_is_fail_open_none` test proves fail-open (returns `None`, no panic).

- [ ] **Step 6: Lint + format**

Run: `cargo fmt && cargo clippy -p apollo-mcp-server --all-targets -- --deny warnings`
Expected: clean (no unused `PathBuf` import warnings).

- [ ] **Step 7: Commit**

```bash
git add crates/apollo-mcp-server/src
git commit -m "feat(air-311): plumb CacheConfig; build store backend from config

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 6: Update the changeset

**Files:**
- Modify: `.changeset/embedding_cache.md`

- [ ] **Step 1: Rewrite the changeset body**

Replace the body of `.changeset/embedding_cache.md` (keep the `default: minor` frontmatter) with:

```markdown
---
default: minor
---

# Cache semantic-search embeddings across restarts (SQLite or Postgres)

When `introspection.search.semantic.cache` is set, operation embeddings are
persisted and reused on later starts instead of being recomputed. Two backends
are supported: `sqlite` (a local file) and `postgres` (a shared database that
lets every MCP replica reuse one durable cache). Vectors are content-addressed
by the SHA-256 of each operation's document text, so only added or changed
operations are re-embedded; the Postgres backend additionally keys rows by the
exact `(model_id, dim, dtype, doc_builder_ver)` generation tuple, so a model,
dimensionality, or document-format change transparently starts a new generation
without deleting or colliding with existing rows. The cache is fail-open: any
connection or I/O error falls back to embedding from scratch. Unset = previous
behavior (embed on every start).
```

- [ ] **Step 2: Commit**

```bash
git add .changeset/embedding_cache.md
git commit -m "docs(air-311): changeset for postgres embedding cache

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Final verification

- [ ] **Full workspace build + test + lint**

Run:
```bash
cargo fmt --check
cargo clippy --all-targets -- --deny warnings
cargo test
```
Expected: all green; the four `PostgresCache` integration tests reported `ignored` (no DB in the default run).

- [ ] **(If Docker available) run the Postgres integration tests**

```bash
docker run --rm -d -e POSTGRES_PASSWORD=pw -p 5432:5432 --name emb-pg postgres:16 && sleep 3
EMBEDDING_CACHE_DATABASE_URL="host=localhost user=postgres password=pw dbname=postgres" \
  cargo test -p apollo-schema-search -- --ignored
docker rm -f emb-pg
```
Expected: `roundtrip_put_then_get`, `generations_are_isolated_without_deletion`, `put_batch_rejects_wrong_length`, `concurrent_put_is_idempotent` all PASS.

---

## Follow-up (separate plan, `constellation-runtime` repo)

Not in scope here; tracked for the deploy plan:
1. `embedding-db.yaml` — single-replica Postgres `StatefulSet` mirroring `keycloak-db.yaml` (own RWO PVC + headless Service), gated on `embeddingCache.enabled`.
2. `deployment.yaml` — add `EMBEDDING_CACHE_DATABASE_URL` (password from Secret) to the `mcp` container and set `introspection.search.semantic.cache_url: ${env.EMBEDDING_CACHE_DATABASE_URL}` in its `/config`.
3. Keep the `startupProbe` tolerating the first cold embed (~140 s).
4. **Cold-start coordination (chosen: scenario 1 only):** set the runtime Deployment rollout to one-at-a-time (`maxSurge: 1`) so a single cold-embedder warms the shared cache before the next pod starts. (A *runtime* schema-change rebuild across live replicas is not serialized by this — accepted for now.)
5. `helm lint` + `helm template` + kind smoke; offline fail-open smoke (`docker run --network=none` → server still comes up lexical + in-memory semantic).
6. Decide in-cluster StatefulSet vs managed Postgres (connection-string-only; no code impact).

**Future (see spec "Future work"):** option B — a session-scoped Postgres advisory lock keyed by the generation tuple so exactly one replica embeds in *both* startup and runtime-rebuild scenarios. Revisit if the concurrent cold-embed resource spike (N × ~2.5 GB) hurts as replicas scale.
