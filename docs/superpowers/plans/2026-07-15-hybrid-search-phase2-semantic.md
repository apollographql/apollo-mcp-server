# Hybrid Search — Phase 2: Semantic + Hybrid (RRF) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax. Per the requester's standing preference, STOP after each task for review before continuing.

**Goal:** Add semantic (dense-vector) search alongside the Phase 1 operation-anchored BM25, fused with Reciprocal Rank Fusion, in a new `apollo-schema-search` crate, wired into the `Search` MCP tool with graceful degradation to lexical-only.

**Architecture:** A new `apollo-schema-search` crate owns the semantic half and the fusion layer: an `Embedder` trait (fastembed/ONNX impl + a deterministic fake for tests), an in-memory brute-force cosine `VectorStore`, a vector `SchemaSearch` backend, and `HybridSearch` which RRF-fuses N `SchemaSearch` backends by `OperationRef`. Both backends index the **same** operation documents (Phase 1's enriched, operation-anchored text), so fusion is clean. The `ort` ONNX dependency is quarantined to this one crate. The `Search` tool composes `[lexical, vector]` behind `HybridSearch`; if the embedder can't initialize, it degrades to lexical-only.

**Tech Stack:** Rust (edition 2024, 1.92), `fastembed` (+ `ort` `load-dynamic`), `apollo-schema-index` (Phase 1), `apollo-compiler`, `insta`, `rstest`.

## Global Constraints

- Rust edition **2024**, rust-version **1.92.0** (workspace-pinned; do not bump).
- Clippy `deny`: `unwrap_used`, `expect_used`, `panic`, `exit`, `indexing_slicing` — none in non-test code (tests may use them). CI `--deny warnings`.
- **80% patch coverage.** Keep the fast test suite offline and deterministic via a **fake `Embedder`**; the real fastembed model runs only in a **gated** (`#[ignore]`) integration test — never in the default `cargo test`.
- **New dependency** (this phase): `fastembed` with the `ort` `load-dynamic` feature, confined to `crates/apollo-schema-search/Cargo.toml`. No ML deps in `apollo-schema-index` or `apollo-mcp-server`.
- **Model & dimensionality:** `bge-small-en-v1.5` (`EmbeddingModel::BGESmallENV15`), 384-dim (default; config-overridable).
- **All embedding inference runs via `tokio::task::spawn_blocking`** (CPU-bound; must not block the async runtime).
- **Vectors are L2-normalized** at insert and query time, so cosine similarity == dot product.
- **Fusion:** RRF, `k = 60`.
- The semantic backend must honor the same **`scope`** filter as lexical (pre-filter the vector set) and the same **operation documents** (via Phase 1's shared enumeration — Task 2).

---

## File structure

- `crates/apollo-schema-search/` *(new crate)*
  - `Cargo.toml` — deps: `apollo-schema-index` (path), `fastembed`, `thiserror`, `tracing`; dev: `rstest`, `insta`.
  - `src/lib.rs` — module wiring + re-exports.
  - `src/embedder.rs` — `Embedder` trait, `EmbedError`, `FakeEmbedder` (test util).
  - `src/fastembed_embedder.rs` — `FastembedEmbedder` (real ONNX impl).
  - `src/vector_store.rs` — `VectorStore` trait + `InMemoryVectorStore` (brute-force cosine, scope pre-filter).
  - `src/fusion.rs` — `rrf_fuse`.
  - `src/vector_search.rs` — `VectorSearch` (`SchemaSearch` impl over `Embedder` + `VectorStore`).
  - `src/hybrid.rs` — `HybridSearch` (`SchemaSearch` impl fusing N backends).
- `crates/apollo-schema-index/src/lib.rs` *(modify)* — expose `enumerate_operation_documents` (shared corpus source; Task 2).
- `crates/apollo-mcp-server/…` *(modify, Task 6)* — `Search` tool composes `HybridSearch`; config threading for `semantic.*` / `hybrid.rrf_k`; `spawn_blocking`; rebuild both indexes on reload.
- Workspace `Cargo.toml` *(modify)* — add `crates/apollo-schema-search` to members.

---

### Task 1: New crate + deterministic core (traits, cosine store, RRF, fake embedder)

Pure Rust, no ML deps. This is the fully-testable, green foundation.

**Files:**
- Create: `crates/apollo-schema-search/Cargo.toml`, `src/lib.rs`, `src/embedder.rs`, `src/vector_store.rs`, `src/fusion.rs`
- Modify: workspace `Cargo.toml` (add member)

**Interfaces produced:**
- `pub trait Embedder: Send + Sync { fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>; fn dimensions(&self) -> usize; }`
- `pub struct FakeEmbedder { dims: usize }` — deterministic hashing-based vectors (tests only, but in `src/` so integration tests can use it).
- `pub trait VectorStore: Send + Sync { fn upsert(&mut self, op: OperationRef, vector: Vec<f32>); fn search(&self, query: &[f32], scope: Option<&str>, limit: usize) -> Vec<Scored<OperationRef>>; }`
- `pub struct InMemoryVectorStore { items: Vec<(OperationRef, Vec<f32>)> }`
- `pub fn rrf_fuse(lists: &[Vec<Scored<OperationRef>>], k: f32) -> Vec<Scored<OperationRef>>`

- [ ] **Step 1: Create the crate manifest + workspace member**

`crates/apollo-schema-search/Cargo.toml`:
```toml
[package]
name = "apollo-schema-search"
authors.workspace = true
edition.workspace = true
license-file.workspace = true
repository.workspace = true
rust-version.workspace = true
version.workspace = true
description = "Hybrid (lexical + semantic) search over GraphQL schema operations"

[dependencies]
apollo-schema-index = { path = "../apollo-schema-index" }
thiserror.workspace = true
tracing.workspace = true

[dev-dependencies]
rstest.workspace = true
insta.workspace = true

[lints]
workspace = true
```
Add `"crates/apollo-schema-search",` to `members` in the root `Cargo.toml`.

- [ ] **Step 2: Write the failing tests** (`src/fusion.rs`, `src/vector_store.rs`, `src/embedder.rs` test modules)

`src/fusion.rs`:
```rust
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
    out.sort_by(|a, b| b.score().partial_cmp(&a.score()).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_schema_index::OperationType;

    fn op(name: &str) -> OperationRef {
        OperationRef { operation_type: OperationType::Query, field_name: name.into(), return_type: None, arg_types: vec![], scope: None }
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
}
```

`src/vector_store.rs`:
```rust
use apollo_schema_index::{OperationRef, Scored};

pub trait VectorStore: Send + Sync {
    fn upsert(&mut self, op: OperationRef, vector: Vec<f32>);
    fn search(&self, query: &[f32], scope: Option<&str>, limit: usize) -> Vec<Scored<OperationRef>>;
}

#[derive(Default)]
pub struct InMemoryVectorStore {
    items: Vec<(OperationRef, Vec<f32>)>,
}

impl InMemoryVectorStore {
    pub fn new() -> Self { Self::default() }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() { *x /= norm; }
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

    fn search(&self, query: &[f32], scope: Option<&str>, limit: usize) -> Vec<Scored<OperationRef>> {
        if limit == 0 { return Vec::new(); }
        let mut q = query.to_vec();
        normalize(&mut q);
        let mut scored: Vec<Scored<OperationRef>> = self
            .items
            .iter()
            .filter(|(op, _)| scope.is_none_or(|s| op.scope.as_deref() == Some(s)))
            .map(|(op, v)| Scored::new(op.clone(), dot(&q, v)))
            .collect();
        scored.sort_by(|a, b| b.score().partial_cmp(&a.score()).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_schema_index::OperationType;

    fn op(name: &str, scope: Option<&str>) -> OperationRef {
        OperationRef { operation_type: OperationType::Query, field_name: name.into(), return_type: None, arg_types: vec![], scope: scope.map(str::to_string) }
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
```

`src/embedder.rs`:
```rust
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
pub struct FakeEmbedder { dims: usize }

impl FakeEmbedder {
    pub fn new(dims: usize) -> Self { Self { dims } }
}

impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|t| {
            let mut v = vec![0.0f32; self.dims];
            for tok in t.split_whitespace() {
                let h = tok.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
                let idx = (h as usize) % self.dims;
                // indexing guarded by modulo; use get_mut to satisfy indexing_slicing lint
                if let Some(slot) = v.get_mut(idx) { *slot += 1.0; }
            }
            v
        }).collect())
    }
    fn dimensions(&self) -> usize { self.dims }
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
```

`src/lib.rs`:
```rust
//! Hybrid (lexical + semantic) search over GraphQL schema operations.
mod embedder;
mod fusion;
mod vector_store;

pub use embedder::{EmbedError, Embedder, FakeEmbedder};
pub use fusion::rrf_fuse;
pub use vector_store::{InMemoryVectorStore, VectorStore};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p apollo-schema-search`
Expected: PASS (fusion 2, vector_store 3, embedder 1).
Run: `cargo clippy -p apollo-schema-search --all-targets -- --deny warnings` → clean.

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt --all
git add crates/apollo-schema-search/ Cargo.toml
git commit -m "feat(air-311): apollo-schema-search crate — Embedder/VectorStore/RRF core"
```

---

### Task 2: Expose shared operation-document enumeration from `apollo-schema-index`

Both backends must index the **same** operation documents. Extract the per-operation text construction (currently private inside `write_operation_docs`) into a public API the vector backend can reuse.

**Files:** Modify `crates/apollo-schema-index/src/lib.rs`

**Interfaces produced:**
- `pub struct OperationDocument { pub op: OperationRef, pub text: String }` — `text` is the `expand_identifiers`-processed enriched document (operation name + args + return type + description + flattened return-type fields), i.e. exactly what BM25 tokenizes and the embedder will embed.
- `pub fn enumerate_operation_documents(schema: &Valid<Schema>, root_types: EnumSet<OperationType>, flatten_depth: usize) -> Vec<OperationDocument>`

- [ ] **Step 1: Write the failing test**
```rust
#[rstest]
fn enumerate_operation_documents_yields_operations_with_text() {
    let schema = Schema::parse(NOISE_SCHEMA, "noise.graphql").unwrap().validate().unwrap();
    let docs = enumerate_operation_documents(&schema, OperationType::Query | OperationType::Mutation, 2);
    let target = docs.iter().find(|d| d.op.field_name == "userByEmail").expect("op present");
    assert!(target.text.contains("user") && target.text.contains("email"));
}
```

- [ ] **Step 2: Run → fails** (`enumerate_operation_documents` missing).

- [ ] **Step 3: Implement** — refactor `write_operation_docs` so the text/`OperationRef` construction lives in `enumerate_operation_documents`, and the Tantivy writer iterates its output. The BM25 document fields must be byte-for-byte what they were (so existing snapshots don't change). Concretely: `enumerate_operation_documents` reproduces the per-field logic from `write_operation_docs` (derive_scope → bare name; `expand_identifiers` over name/args/return-type/description/flatten) and returns `OperationDocument { op, text }` where `text` is the concatenation of those analyzed fields; `write_operation_docs` then calls it and writes each field. Keep `operation_name`/`arg_names`/etc. as separate Tantivy fields (do not collapse) — `OperationDocument.text` is the *embedding* source (joined), while Tantivy still gets its per-field structure. Re-run the `search` snapshot to confirm BM25 output is unchanged.

- [ ] **Step 4: Run → passes**; `cargo test -p apollo-schema-index` all green; snapshot unchanged.
- [ ] **Step 5: Commit** `feat(air-311): expose enumerate_operation_documents for shared corpus`.

---

### Task 3: Vector `SchemaSearch` backend

**Files:** Create `crates/apollo-schema-search/src/vector_search.rs`; update `src/lib.rs`.

**Interfaces produced:**
- `pub struct VectorSearch { store: InMemoryVectorStore, embedder: Arc<dyn Embedder> }`
- `pub fn build(schema, root_types, flatten_depth, embedder: Arc<dyn Embedder>) -> Result<VectorSearch, EmbedError>` — enumerates operation documents (Task 2), embeds them in batch, upserts into the store.
- `impl SchemaSearch for VectorSearch` — embeds the query, delegates to `store.search(query_vec, scope, limit)`.

- [ ] **Step 1: Failing test** (with `FakeEmbedder`): build over `NOISE_SCHEMA`, search "user email", assert an `OperationRef` comes back and scope filter works.
- [ ] **Step 2: Run → fails.**
- [ ] **Step 3: Implement** `build` (enumerate → `embedder.embed(texts)` → `upsert` each with its `OperationRef`) and the `SchemaSearch` impl (`embed(&[query])` → `store.search`). Guard `limit==0`.
- [ ] **Step 4: Run → passes**; clippy clean.
- [ ] **Step 5: Commit** `feat(air-311): vector SchemaSearch backend (in-memory cosine)`.

---

### Task 4: `HybridSearch` (RRF over backends)

**Files:** Create `crates/apollo-schema-search/src/hybrid.rs`; update `src/lib.rs`.

**Interfaces produced:**
- `pub struct HybridSearch { backends: Vec<Box<dyn SchemaSearch>>, rrf_k: f32 }`
- `impl SchemaSearch for HybridSearch::search(query, scope, limit)` — asks each backend for a candidate pool (≥ limit), `rrf_fuse`, truncate to `limit`.

- [ ] **Step 1: Failing test** — a `HybridSearch` of two fake `SchemaSearch` stubs; assert an operation ranked high in both lands on top after fusion; assert single-backend passthrough.
- [ ] **Step 2: Run → fails.**
- [ ] **Step 3: Implement.** Each backend queried with a candidate pool = `max(limit, POOL)` (const, e.g. 50); collect the ranked lists; `rrf_fuse(&lists, self.rrf_k)`; `truncate(limit)`. A backend returning `Err` is logged and skipped (degradation), not fatal.
- [ ] **Step 4: Run → passes**; clippy clean.
- [ ] **Step 5: Commit** `feat(air-311): HybridSearch RRF fusion over backends`.

---

### Task 5: Real fastembed `Embedder` (ONNX) — the dependency lands here

**Files:** Create `crates/apollo-schema-search/src/fastembed_embedder.rs`; modify `Cargo.toml`; update `src/lib.rs`.

**Interfaces produced:**
- `pub struct FastembedEmbedder { model: Mutex<fastembed::TextEmbedding>, dims: usize }`
- `pub fn new(model_name: &str, inference_threads: usize) -> Result<FastembedEmbedder, EmbedError>`
- `impl Embedder for FastembedEmbedder`

- [ ] **Step 1: Add deps** to `crates/apollo-schema-search/Cargo.toml`:
```toml
fastembed = { version = "<pin latest 5.x>", default-features = false, features = ["ort-load-dynamic"] }
```
(Confirm the exact feature name for `ort` `load-dynamic` passthrough against the fastembed version — the crate exposes ONNX-Runtime linking via an `ort-load-dynamic`/`ort-download-binaries` feature set. Use `load-dynamic` so the `.so` is provided at runtime via `ORT_DYLIB_PATH` in Phase 3.)

- [ ] **Step 2: Implement** the embedder:
```rust
use crate::embedder::{EmbedError, Embedder};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::sync::Mutex;

pub struct FastembedEmbedder { model: Mutex<TextEmbedding>, dims: usize }

impl FastembedEmbedder {
    pub fn new(model_name: &str, inference_threads: usize) -> Result<Self, EmbedError> {
        let model = parse_model(model_name)?; // map "bge-small-en-v1.5" -> EmbeddingModel::BGESmallENV15
        let dims = model_dims(&model);        // 384 for bge-small / MiniLM
        let embedding = TextEmbedding::try_new(
            TextInitOptions::new(model)
                .with_show_download_progress(false)
                .with_intra_threads(inference_threads),
        ).map_err(|e| EmbedError::Init(e.to_string()))?;
        Ok(Self { model: Mutex::new(embedding), dims })
    }
}

impl Embedder for FastembedEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut guard = self.model.lock().map_err(|e| EmbedError::Inference(e.to_string()))?;
        guard.embed(texts.to_vec(), None).map_err(|e| EmbedError::Inference(e.to_string()))
    }
    fn dimensions(&self) -> usize { self.dims }
}
```
Provide `parse_model` (name → `EmbeddingModel`, error on unknown) and `model_dims`. *Confirm `embed`'s exact arg (owned `Vec<String>` vs `&[String]`) and return type against the pinned fastembed version; adjust the `.to_vec()` accordingly.* For fully offline/air-gapped provisioning use `try_new_from_user_defined` with bundled ONNX+tokenizer files — deferred to Phase 3 packaging; for dev/CI the default cache-download path is acceptable.

- [ ] **Step 3: Gated real-model integration test** — `#[ignore]`d test (or `--features slow-tests`) that builds `FastembedEmbedder::new("bge-small-en-v1.5", 1)`, embeds two strings, asserts `dims == 384` and that a semantically-close pair scores higher than a far pair. Document that it downloads the model on first run and is excluded from default `cargo test`.
- [ ] **Step 4: Verify** the default `cargo test -p apollo-schema-search` still passes **without** network/model (fake embedder only); the real test only runs with `--ignored`/feature. clippy clean.
- [ ] **Step 5: Commit** `feat(air-311): fastembed ONNX embedder (load-dynamic)`.

---

### Task 6: Wire hybrid search into the `Search` MCP tool (integration)

**Files:** Modify `crates/apollo-mcp-server/`: `introspection/tools/search.rs`, `runtime/introspection.rs`, `main.rs`, `server.rs`, `server/states.rs`, `server/states/starting.rs`, `server/states/running.rs`; add `apollo-schema-search` to `apollo-mcp-server/Cargo.toml`.

**Behavior:**
- New config under `SearchConfig`: `semantic.enabled` (bool, default true when search enabled), `semantic.model` (default `"bge-small-en-v1.5"`), `semantic.inference_threads` (default 1), `hybrid.rrf_k` (default 60). Thread through the builder layers exactly as Phase 1 threaded `flatten_depth` (mirror `search_flatten_depth`).
- `Search::new`: if `semantic.enabled`, attempt to build `FastembedEmbedder` + `VectorSearch`; compose `HybridSearch[lexical, vector]`. **On embedder init failure, log a warning and fall back to lexical-only** (`HybridSearch[lexical]` or the bare `SchemaIndex`). The tool must still come up.
- `execute`: unchanged call shape (`search(&query, scope, k)`) — it already targets the `SchemaSearch` trait, which `HybridSearch` implements. **Wrap the search call in `tokio::task::spawn_blocking`** (embedding is CPU-bound). Tree-shaking unchanged.
- `rebuild` (reload): rebuild both the lexical index and the vector store from the new schema.

- [ ] **Step 1: Failing tests** — (a) `SearchConfig` defaults include `semantic.enabled=true`, `semantic.model="bge-small-en-v1.5"`, `hybrid.rrf_k=60`; (b) a `Search` built with a **fake/failing embedder injected** degrades to lexical-only and still returns results (inject via a test constructor that takes an `Arc<dyn Embedder>`; production path uses fastembed).
- [ ] **Step 2: Run → fails.**
- [ ] **Step 3: Implement** the config fields + builder-layer threading (mirror Phase 1's `flatten_depth` sites), the `HybridSearch` composition with graceful degradation, `spawn_blocking` around inference, and `rebuild` rebuilding both. Add `apollo-schema-search` as a dependency of `apollo-mcp-server`. Provide a `Search::new` that constructs the embedder, plus a test-only constructor accepting an injected `Arc<dyn Embedder>` so degradation and hybrid behavior are testable offline.
- [ ] **Step 4: Run → passes**; `cargo test --workspace`, `cargo clippy --all-targets -- --deny warnings`, `cargo +nightly fmt --all` all clean. Review `insta` snapshots (hybrid ranking may reorder tool output — accept sane changes).
- [ ] **Step 5: Commit** `feat(air-311): compose hybrid search in the MCP tool with graceful degradation`.

---

## Self-review

- **Spec coverage:** `Embedder`/fastembed (Tasks 1,5) ✓; in-memory cosine `VectorStore` (Task 1) ✓; RRF `HybridSearch` (Tasks 1,4) ✓; shared operation corpus (Task 2) ✓; vector backend honoring scope (Tasks 1,3) ✓; graceful degradation + `spawn_blocking` + config threading + rebuild (Task 6) ✓; ONNX quarantined to the new crate + `load-dynamic` (Task 5) ✓; fake-embedder-fast-tests / gated real-model test (Tasks 1,5) ✓. Docker packaging (bake model + `.so`) is **Phase 3**, out of scope here.
- **Placeholder check:** the only "confirm against pinned version" notes are for the exact fastembed `embed()` arg type and the `ort` feature name — legitimately version-specific and called out at the site, with the intended shape given. Not vague placeholders.
- **Type consistency:** `Embedder::embed(&[String]) -> Result<Vec<Vec<f32>>>`, `VectorStore::search(&[f32], Option<&str>, usize)`, `SchemaSearch::search(&str, Option<&str>, usize) -> Vec<Scored<OperationRef>>`, and `rrf_fuse(&[Vec<Scored<OperationRef>>], f32)` are used consistently across Tasks 1–6.

## Follow-ups (Phase 3)

- Dockerfile: bake the model + `libonnxruntime.so` into the `apollo-mcp-server` image; `ort` `load-dynamic` + `ORT_DYLIB_PATH`; disable fastembed runtime download (bundle via `try_new_from_user_defined` or a baked cache dir); multi-arch; bump pod memory + readiness-probe timing in `constellation-runtime`.
