# SQLite Embedding Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist operation embeddings in a local SQLite file so the semantic corpus is embedded once (slow) and reloaded on subsequent starts (fast), instead of re-embedding on every startup.

**Architecture:** A new `EmbeddingCache` in `apollo-schema-search` wraps a `rusqlite` connection. Vectors are stored as raw little-endian `f32` BLOBs, **content-addressed** by `SHA-256(operation document text)`. `VectorSearch::build` looks each operation up by content key, embeds only the misses, and writes them back — so an unchanged operation is reused and a changed one is re-embedded (incremental). A one-row `meta` table guards against model/dimensionality/format changes and wipes the vectors when those change. The cache is opt-in via config and fails open: any I/O or corruption error degrades to embedding-from-scratch, never a crash.

**Tech Stack:** Rust (edition 2024), `rusqlite` (SQLite, `bundled` feature — static, no system dep), `sha2` + `hex` for the content key, existing `Embedder`/`VectorStore`/`SchemaSearch` seams.

## Global Constraints

- Rust edition **2024**, rust-version **1.92.0** (workspace `Cargo.toml`).
- Lints are **deny**: no `unwrap`, `expect`, `panic`, or indexing/slicing in library code (`clippy.toml` / CI `--deny warnings`). All code below is written to satisfy this.
- New/modified code needs **80% patch coverage**; every task is TDD.
- `rusqlite` MUST use the `bundled` feature so SQLite is statically linked (on-prem / distroless has no system SQLite and no shell). No `load_extension`, no `sqlite-vec`.
- Vectors are stored as **raw little-endian `f32`** (`dim × 4` bytes); the `meta.dtype` value is `"f32le"`.
- The cache is **fail-open**: on any `CacheError` the server logs a warning and proceeds as if the cache were absent (embed everything). It must never propagate an error that aborts search construction.
- Run tests with `cargo test -p <crate>`; format with `cargo fmt`; lint with `cargo clippy --all-targets -- --deny warnings`.

---

### Task 1: Add dependencies to `apollo-schema-search`

**Files:**
- Modify: `crates/apollo-schema-search/Cargo.toml`

**Interfaces:**
- Produces: the `rusqlite`, `sha2`, `hex` crates available to `apollo-schema-search` for later tasks.

- [ ] **Step 1: Add dependencies**

In `crates/apollo-schema-search/Cargo.toml`, under `[dependencies]`, add:

```toml
rusqlite = { version = "0.32", features = ["bundled"] }
sha2 = "0.10.9"
hex = "0.4.3"
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p apollo-schema-search`
Expected: compiles (rusqlite `bundled` compiles vendored SQLite on first build — may take a minute).

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-schema-search/Cargo.toml Cargo.lock
git commit -m "build(air-311): add rusqlite/sha2/hex for the embedding cache"
```

---

### Task 2: `EmbeddingCache` module (storage + serialization + meta guard)

**Files:**
- Create: `crates/apollo-schema-search/src/embedding_cache.rs`
- Modify: `crates/apollo-schema-search/src/lib.rs` (add `mod` + `pub use`)

**Interfaces:**
- Consumes: `rusqlite`, `sha2`, `hex` (Task 1).
- Produces:
  - `pub const DOC_BUILDER_VERSION: i64` — bump when `enumerate_operation_documents` text format changes.
  - `pub fn doc_key(text: &str) -> String` — SHA-256 hex content key.
  - `pub struct EmbeddingCache` with:
    - `pub fn open(path: &std::path::Path, model_id: &str, dim: usize, doc_builder_ver: i64) -> Result<Self, CacheError>`
    - `pub fn get(&self, key: &str) -> Result<Option<Vec<f32>>, CacheError>`
    - `pub fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError>`
  - `pub enum CacheError` (impls `std::error::Error` via `thiserror`).

- [ ] **Step 1: Write the module with failing tests**

Create `crates/apollo-schema-search/src/embedding_cache.rs`:

```rust
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
    pub fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError> {
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
        let path = std::env::temp_dir().join(format!(
            "emb_cache_{}_{}_{}.db",
            tag,
            std::process::id(),
            n
        ));
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
            cache.put_batch(&[("k".to_string(), vec![0.5, 0.5])]).unwrap();
        }
        let cache = EmbeddingCache::open(&path, "m", 2, 1).unwrap();
        assert_eq!(cache.get("k").unwrap(), Some(vec![0.5, 0.5]));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrong_length_blob_is_error() {
        // A 2-float vector stored, then read back as if dim=3 -> length error.
        let path = temp_db("badlen");
        {
            let mut c = EmbeddingCache::open(&path, "m", 2, 1).unwrap();
            c.put_batch(&[("k".to_string(), vec![1.0, 2.0])]).unwrap();
        }
        let c = EmbeddingCache::open(&path, "m", 3, 1).unwrap(); // dim mismatch also wipes...
        // meta guard wiped it (dim changed), so it's simply a miss:
        assert_eq!(c.get("k").unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn doc_key_is_stable_and_content_addressed() {
        assert_eq!(doc_key("send message"), doc_key("send message"));
        assert_ne!(doc_key("send message"), doc_key("send messages"));
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/apollo-schema-search/src/lib.rs` add the module and re-exports:

```rust
mod embedding_cache;
```
and in the `pub use` block:
```rust
pub use embedding_cache::{CacheError, DOC_BUILDER_VERSION, EmbeddingCache, doc_key};
```

- [ ] **Step 3: Run the tests, expect FAIL then PASS**

Run: `cargo test -p apollo-schema-search embedding_cache`
Expected: compiles and all 5 tests PASS.

- [ ] **Step 4: Lint + format**

Run: `cargo fmt && cargo clippy -p apollo-schema-search --all-targets -- --deny warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-schema-search/src/embedding_cache.rs crates/apollo-schema-search/src/lib.rs
git commit -m "feat(air-311): SQLite embedding cache (content-addressed, meta-guarded)"
```

---

### Task 3: Use the cache in `VectorSearch::build`

**Files:**
- Modify: `crates/apollo-schema-search/src/vector_search.rs`

**Interfaces:**
- Consumes: `EmbeddingCache`, `doc_key` (Task 2); `enumerate_operation_documents`, `OperationDocument` (apollo-schema-index).
- Produces: new signature
  `VectorSearch::build(schema, root_types, flatten_depth, embedder: Arc<dyn Embedder>, cache: Option<&mut EmbeddingCache>) -> Result<Self, EmbedError>`
  (adds the trailing `cache` argument; callers in Task 4 and the existing tests pass `None`).

- [ ] **Step 1: Update the failing test first**

In `crates/apollo-schema-search/src/vector_search.rs`, add to the top of the file's `use` section:
```rust
use crate::embedding_cache::{EmbeddingCache, doc_key};
use apollo_schema_index::OperationDocument;
```
Add a counting embedder + a cache-reuse test to the `tests` module:
```rust
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingEmbedder {
        inner: FakeEmbedder,
        embedded: Arc<AtomicUsize>,
    }
    impl Embedder for CountingEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, crate::EmbedError> {
            self.embedded.fetch_add(texts.len(), Ordering::SeqCst);
            self.inner.embed(texts)
        }
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }
    }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "vs_cache_{}_{}_{}.db",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn second_build_reuses_cache_and_embeds_nothing() {
        let schema = Schema::parse(SCHEMA, "s.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let roots = OperationType::Query | OperationType::Mutation;
        let path = tmp_db("reuse");

        // First build: cold cache -> embeds all ops.
        let count1 = Arc::new(AtomicUsize::new(0));
        {
            let mut cache = EmbeddingCache::open(&path, "fake", 64, 1).unwrap();
            let emb = Arc::new(CountingEmbedder {
                inner: FakeEmbedder::new(64),
                embedded: count1.clone(),
            });
            VectorSearch::build(&schema, roots, 2, emb, Some(&mut cache)).unwrap();
        }
        assert!(count1.load(Ordering::SeqCst) > 0, "first build should embed");

        // Second build: warm cache -> embeds nothing, still returns results.
        let count2 = Arc::new(AtomicUsize::new(0));
        let vs = {
            let mut cache = EmbeddingCache::open(&path, "fake", 64, 1).unwrap();
            let emb = Arc::new(CountingEmbedder {
                inner: FakeEmbedder::new(64),
                embedded: count2.clone(),
            });
            VectorSearch::build(&schema, roots, 2, emb, Some(&mut cache)).unwrap()
        };
        assert_eq!(count2.load(Ordering::SeqCst), 0, "second build must be all cache hits");
        assert!(!vs.search("user by email", None, 10).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }
```
Also update the existing `index()` test helper to pass the new arg:
```rust
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
            None,
        )
        .unwrap()
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p apollo-schema-search vector_search`
Expected: FAIL — `build` takes 4 args, tests pass 5.

- [ ] **Step 3: Implement the cache-aware `build`**

Replace the body of `VectorSearch::build` with:
```rust
    #[tracing::instrument(skip_all, name = "embedding")]
    pub fn build(
        schema: &Valid<Schema>,
        root_types: EnumSet<OperationType>,
        flatten_depth: usize,
        embedder: Arc<dyn Embedder>,
        mut cache: Option<&mut EmbeddingCache>,
    ) -> Result<Self, crate::EmbedError> {
        let docs = enumerate_operation_documents(schema, root_types, flatten_depth);
        let mut store = InMemoryVectorStore::new();
        let mut miss_keys: Vec<String> = Vec::new();
        let mut miss_docs: Vec<OperationDocument> = Vec::new();

        // 1. Content-addressed lookup: reuse cached vectors, collect misses.
        for doc in docs {
            let key = doc_key(&doc.text);
            let hit = match cache.as_deref() {
                Some(c) => c.get(&key).unwrap_or(None), // a cache read error == a miss
                None => None,
            };
            match hit {
                Some(vector) => store.upsert(doc.op, vector),
                None => {
                    miss_keys.push(key);
                    miss_docs.push(doc);
                }
            }
        }

        // 2. Embed only the misses (the expensive step), persist, and store.
        if miss_docs.is_empty() {
            tracing::info!(reused = store_len(&store), "Loaded all embeddings from cache");
        } else {
            let texts: Vec<String> = miss_docs.iter().map(|d| d.text.clone()).collect();
            let start = std::time::Instant::now();
            let vectors = embedder.embed(&texts)?;
            tracing::info!(
                embedded = texts.len(),
                reused = store_len(&store),
                "Embedded corpus in {:.2?}",
                start.elapsed()
            );
            if let Some(c) = cache.as_deref_mut() {
                let entries: Vec<(String, Vec<f32>)> =
                    miss_keys.into_iter().zip(vectors.iter().cloned()).collect();
                if let Err(e) = c.put_batch(&entries) {
                    tracing::warn!("failed to persist embeddings to cache: {e}");
                }
            }
            for (doc, vector) in miss_docs.into_iter().zip(vectors) {
                store.upsert(doc.op, vector);
            }
        }
        Ok(Self { store, embedder })
    }
```
Add a tiny helper near the top of `impl VectorSearch` (or a free `fn`) so the log's `reused` count doesn't need a public store accessor — count what's already in the store:
```rust
fn store_len(_store: &InMemoryVectorStore) -> usize {
    // InMemoryVectorStore has no len(); the reused count is docs.len() - misses.
    // Simplest: track counts inline instead. (See note.)
    0
}
```
> NOTE for the implementer: `InMemoryVectorStore` exposes no `len()`. Rather than add one, track the reused count with a local `let mut reused = 0usize;` incremented in the hit branch, and log `reused` directly — delete the `store_len` helper. Use whichever is cleaner; do not add indexing or a panic. The tests do not assert on the log, only on embed counts.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p apollo-schema-search vector_search`
Expected: PASS (both the reuse test and the existing `returns_operations_and_carries_scope` / `scope_restricts_results` / `zero_limit_is_empty`).

- [ ] **Step 5: Lint, format, commit**

```bash
cargo fmt && cargo clippy -p apollo-schema-search --all-targets -- --deny warnings
git add crates/apollo-schema-search/src/vector_search.rs
git commit -m "feat(air-311): reuse cached embeddings in VectorSearch::build (incremental)"
```

---

### Task 4: Open the cache in `build_backend` (fail-open)

**Files:**
- Modify: `crates/apollo-mcp-server/src/introspection/tools/search.rs`

**Interfaces:**
- Consumes: `VectorSearch::build(..., Option<&mut EmbeddingCache>)` (Task 3); `EmbeddingCache::open`, `DOC_BUILDER_VERSION` (Task 2).
- Produces: `build_backend` gains two params — `cache_path: Option<&std::path::Path>` and `model_id: &str`. `Search::new_with_embedder` gains `cache_path: Option<PathBuf>` and threads it (with `semantic_model` as `model_id`) into `build_backend`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `search.rs` (mirrors the existing `hybrid_with_fake_embedder_returns_results` test, which calls `new_with_embedder`):
```rust
    #[rstest]
    #[tokio::test]
    async fn build_with_cache_path_reuses_on_rebuild(schema: Valid<Schema>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = std::env::temp_dir().join(format!("search_cache_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("emb.db");
        let _ = std::fs::remove_file(&db);
        let schema = Arc::new(RwLock::new(schema));

        // A fake embedder is fine; we only assert the tool builds and searches with a cache path set.
        let make = || {
            Search::new_with_embedder(
                schema.clone(), false, 1, 2, 15_000_000, 10, 50, false, None,
                Some(Arc::new(apollo_schema_search::FakeEmbedder::new(64))),
                60.0,
                Some(db.clone()),
            )
            .expect("build with cache")
        };
        let _first = make();          // populates the cache file
        assert!(db.exists(), "cache file should be created");
        let second = make();          // reads it back
        let result = second
            .execute(Input { terms: vec!["User".to_string()], limit: None, scope: None })
            .await
            .expect("search");
        assert!(!result.is_error.unwrap_or(false));
        let _ = std::fs::remove_file(&db);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p apollo-mcp-server --lib introspection::tools::search::tests::build_with_cache_path_reuses_on_rebuild`
Expected: FAIL — `new_with_embedder` doesn't take a cache-path arg yet.

- [ ] **Step 3: Thread the cache path through `build_backend` and `new_with_embedder`**

At the top of `search.rs`, extend the import:
```rust
use apollo_schema_search::{DOC_BUILDER_VERSION, Embedder, EmbeddingCache, FastembedEmbedder, HybridSearch, VectorSearch};
use std::path::{Path, PathBuf};
```
Change `build_backend`'s signature and the embedder branch:
```rust
fn build_backend(
    schema: &Valid<Schema>,
    allow_mutations: bool,
    flatten_depth: usize,
    index_memory_bytes: usize,
    embedder: Option<Arc<dyn Embedder>>,
    rrf_k: f32,
    cache_path: Option<&Path>,
    model_id: &str,
) -> Result<Box<dyn SchemaSearch + Send + Sync>, IndexingError> {
    let root_types = if allow_mutations {
        OperationType::Query | OperationType::Mutation
    } else {
        OperationType::Query.into()
    };
    let index = SchemaIndex::new(schema, root_types, flatten_depth, index_memory_bytes)?;
    match embedder {
        Some(emb) => {
            // Open the cache if configured; a failure is non-fatal (embed from scratch).
            let mut cache = cache_path.and_then(|p| {
                match EmbeddingCache::open(p, model_id, emb.dimensions(), DOC_BUILDER_VERSION) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        warn!("embedding cache disabled (open failed at {p:?}): {e}");
                        None
                    }
                }
            });
            match VectorSearch::build(schema, root_types, flatten_depth, emb, cache.as_mut()) {
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
        None => Ok(Box::new(index)),
    }
}
```
Add a `cache_path: Option<PathBuf>` field to the `Search` struct (next to `embedder`), store it in `new_with_embedder`, add the parameter to both `new_with_embedder` and `new`, and thread `semantic_model` as `model_id`:
- In `new`: add trailing param `semantic_cache_path: Option<PathBuf>` and pass it to `new_with_embedder` (the model id is the existing `semantic_model`).
- In `new_with_embedder`: add trailing param `cache_path: Option<PathBuf>`; store `cache_path.clone()` on the struct; the initial `build_backend(...)` call passes `cache_path.as_deref()` and the model id. Because `new_with_embedder` doesn't currently receive the model string, add a `model_id: String` field set from `new` (pass `semantic_model.to_string()`); default it to `""` in the two existing tests that call `new_with_embedder` directly — OR simpler: give `new_with_embedder` a `model_id: &str` param. Use `&str`.
- Update `Search::rebuild` (which also calls `build_backend`) to pass `self.cache_path.as_deref()` and `&self.model_id`.

Update the three existing `new_with_embedder` test call sites (`search_tool`, `degrades_to_lexical_when_embedder_fails`, `hybrid_with_fake_embedder_returns_results`, `unknown_scope_falls_back_to_global`, and the limit/referencing tests) to pass the two new trailing args `"fake"` (model id) and `None` (cache path). Example for `search_tool`:
```rust
        let search = Search::new_with_embedder(
            schema.clone(), false, 1, 2, 15_000_000, 10, 50, false, None,
            None, 60.0, /* model_id */ "fake", /* cache_path */ None,
        )
        .expect("Failed to create search tool");
```
> Keep the parameter ORDER consistent everywhere: `(…, description_hint, embedder, rrf_k, model_id, cache_path)`. Update the `build_with_cache_path_reuses_on_rebuild` test from Step 1 to match this order (it passes `Some(Arc::new(FakeEmbedder))`, `60.0`, `"fake"`, `Some(db)`).

- [ ] **Step 4: Run all search tests to verify pass**

Run: `cargo test -p apollo-mcp-server --lib introspection::tools::search`
Expected: PASS (all existing tests + the new cache test).

- [ ] **Step 5: Lint, format, commit**

```bash
cargo fmt && cargo clippy -p apollo-mcp-server --all-targets -- --deny warnings
git add crates/apollo-mcp-server/src/introspection/tools/search.rs
git commit -m "feat(air-311): open embedding cache in build_backend (fail-open)"
```

---

### Task 5: Config + builder plumbing (`semantic.cache_path`)

**Files:**
- Modify: `crates/apollo-mcp-server/src/runtime/introspection.rs` (add config field)
- Modify: `crates/apollo-mcp-server/src/server.rs` (builder field + wiring — mirrors `semantic_inference_threads`)
- Modify: `crates/apollo-mcp-server/src/main.rs` (read config → builder — mirrors `.semantic_inference_threads(...)`)

**Interfaces:**
- Consumes: `Search::new(..., semantic_model, semantic_inference_threads, rrf_k, semantic_cache_path: Option<PathBuf>)` (Task 4 added the trailing arg to `new`).
- Produces: a YAML key `introspection.search.semantic.cache_path` (string path, default unset = cache disabled).

- [ ] **Step 1: Add the config field + test**

In `runtime/introspection.rs`, add to `SemanticConfig`:
```rust
    /// Path to a SQLite file used to cache operation embeddings across restarts.
    /// Unset = disabled (embed on every start). Relative paths resolve against CWD.
    #[serde(default)]
    pub cache_path: Option<std::path::PathBuf>,
```
and in its `Default`:
```rust
            cache_path: None,
```
Add a test (near existing config tests, or a new `#[test]`) proving it deserializes:
```rust
#[test]
fn semantic_cache_path_parses() {
    let c: SemanticConfig = serde_yaml::from_str(
        "enabled: true\nmodel: bge-small-en-v1.5\ninference_threads: 2\ncache_path: /data/emb.db\n",
    )
    .unwrap();
    assert_eq!(c.cache_path, Some(std::path::PathBuf::from("/data/emb.db")));
}
```
(Use the crate's existing YAML test helper if there is one; otherwise `serde_yaml` is already a dependency of the runtime module — confirm and mirror an adjacent config test.)

- [ ] **Step 2: Run to verify fail, then implement plumbing**

Run: `cargo test -p apollo-mcp-server --lib runtime::introspection`
Expected: FAIL until the field is added; PASS after Step 1.

Then wire it through, mirroring `semantic_inference_threads` exactly:
- In `server.rs`: add a `semantic_cache_path: Option<PathBuf>` builder field + setter, store it on the running state, and pass it as the trailing arg to `Search::new(...)` where `semantic_inference_threads` is already passed.
- In `main.rs`: where the builder is configured (`.semantic_inference_threads(config.introspection.search.semantic.inference_threads)`), add:
  ```rust
  .semantic_cache_path(config.introspection.search.semantic.cache_path.clone())
  ```

- [ ] **Step 3: Full test + clippy gate**

Run:
```bash
cargo test -p apollo-mcp-server -p apollo-schema-search -p apollo-schema-index
cargo clippy --all-targets -- --deny warnings
cargo fmt --check
```
Expected: all pass, no warnings, formatting clean.

- [ ] **Step 4: Add a changeset**

Create `.changeset/embedding_cache.md`:
```markdown
---
default: minor
---

# Cache semantic-search embeddings in SQLite across restarts

When `introspection.search.semantic.cache_path` is set, operation embeddings are
persisted to a local SQLite file and reused on subsequent starts instead of being
recomputed. Vectors are content-addressed by the SHA-256 of each operation's
document text, so only added or changed operations are re-embedded; a model,
dimensionality, or document-format change transparently invalidates the cache.
The cache is fail-open: any I/O or corruption error falls back to embedding from
scratch. Unset = previous behavior (embed on every start).
```

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-mcp-server/src/runtime/introspection.rs crates/apollo-mcp-server/src/server.rs crates/apollo-mcp-server/src/main.rs .changeset/embedding_cache.md
git commit -m "feat(air-311): config + plumbing for semantic.cache_path"
```

---

## Design notes (rationale, not tasks)

- **Why content-addressed keys, not `launch_id`/SDL hash:** keying each vector on `SHA-256(doc_text)` means a changed operation misses and re-embeds while unchanged ones hit — incremental by construction, and identical across the uplink and local-file schema sources. A global schema hash used as an *invalidation* key would wipe the whole cache on any one-operation change (no incrementality) and would require threading the `pub(crate)` `SchemaState.launch_id` across crates. So we don't use it. (A schema-version *fast-path* — "unchanged → skip enumeration" — could be added later, but enumeration + hashing the corpus is milliseconds; the win is skipping *embedding*, which content addressing already delivers.)
- **Why no `sqlite-vec`:** search stays in the existing in-RAM brute-force `InMemoryVectorStore`; SQLite is pure persistence. Avoids shipping/loading a native extension in distroless/on-prem, and at this corpus size (≤ ~31k ops) brute-force in RAM is microseconds — an ANN engine buys nothing. See `docs/superpowers/specs/2026-07-14-hybrid-search-design.md` (Qdrant is the scale-out target, not sqlite-vec).
- **`meta` guard vs content key, division of labor:** the content key handles *per-operation* drift (schema edits, `flatten_depth` — both change `doc_text`). The `meta` row handles *global* invalidation that does NOT change `doc_text`: a different embedding **model** or **dimensionality**, or a change to the stored byte **format** (`dtype`) or the **doc-builder version**. `DOC_BUILDER_VERSION` must be bumped whenever `enumerate_operation_documents` changes what text it emits.
- **Fail-open everywhere:** `EmbeddingCache::open` failure → no cache (embed all); `get` error → treat as miss; `put_batch` error → log + continue. The cache can never break search construction.
- **Memory/disk are non-issues at scale:** on-disk DB ≈ `N × dim × 4` + light overhead (≈ 55 MB at 20× corpus / 384-dim; ≈ 137 MB at 20× / 1024-dim). In-RAM vectors are the same order. The startup win is skipping the ~140 s embed, not saving memory.
