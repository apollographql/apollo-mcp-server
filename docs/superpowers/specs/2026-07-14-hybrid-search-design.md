# AIR-311 — Hybrid Search (BM25 + Semantic) Design

**Status:** design / awaiting review
**Ticket:** AIR-311
**Branch:** `air-311-hybrid-search` (off `tninesling/field-search`)
**Date:** 2026-07-14
**Companion:** [callouts & pitfalls](../notes/2026-07-14-hybrid-search-callouts.md)

## Summary

Add **short-term, in-process hybrid search** to the Apollo MCP Server: keep the
existing BM25 lexical search, add semantic (dense-vector) search, and fuse the two
with Reciprocal Rank Fusion (RRF). The retrieval unit becomes the **operation**
(a root Query/Mutation field), and both backends index the same
operation-anchored, enriched documents so fusion is clean.

This is explicitly a **short-term** solution. Search will eventually be extracted
into a standalone service; the work is structured so that extraction is "lift a
crate," not "untangle a crate." Everything runs in the existing
`apollo-mcp-server` binary/image; `constellation-runtime` consumes it as a
separate container and only bumps the image tag.

## Goals

- Improve tool/operation retrieval quality for agent prompts, especially
  multi-intent prompts where lexical-only search underperforms.
- Keep it **simple to run and easy to deploy on-premise** (including air-gapped).
- Keep the new work **liftable** into the future search service.
- Avoid regressing the existing `execute`/`introspect`/`search` MCP tools.

## Non-goals (short-term)

- A standalone search service (future; this design just makes extraction easy).
- A dedicated vector database such as Qdrant (future `VectorStore` impl / the
  extraction target — not a short-term dependency).
- On-disk persistence of the index (rebuilt in RAM at startup).
- An official/maintained evaluation suite (see Testing & Evaluation).

## Background — current state (verified in-repo)

- **BM25 = Tantivy `0.24.2`**, built **in-RAM** at startup in
  `crates/apollo-schema-index` (`SchemaIndex::new` → `Index::create_in_ram`).
- The index is **field-anchored**: one document per object/interface field and
  per enum value reached by traversing from the root operation types, plus a
  `type_references` graph and an up-walk (`walk_up_to_roots` / `build_leaf_path` /
  `boost_shorter_paths`) to map a field hit back to the operation that reaches it.
- **No re-index on schema hot-reload:** `Running::update_schema`
  (`crates/apollo-mcp-server/src/server/states/running.rs`) swaps the schema but
  never rebuilds the index, so BM25 goes **stale** after a reload. Pre-existing bug.
- **No search abstraction** — Tantivy is hardwired. The MCP `Search` tool
  (`crates/apollo-mcp-server/src/introspection/tools/search.rs`) calls
  `SchemaIndex::search`, truncates to a hardcoded `MAX_SEARCH_RESULTS = 5`, and
  tree-shakes each result into returned SDL.
- **No ML/vector/embedding deps** exist in the workspace today.
- Constraints: edition 2024 / Rust 1.92, `deny(unwrap/expect/panic/indexing_slicing)`,
  80% patch coverage. Tantivy currently runs synchronously inside async handlers.
- **Prior prototyping:** the operation-level hybrid approach and its parameters
  (`bge-small-en-v1.5` / 384-dim, RRF constant 60, rich operation rendering that
  folds in return-type fields) were validated *directionally* in a personal Python
  PoC over the Constellation staging supergraph (~1,207 operations). It is a quick
  experiment, not an official suite; used here only as prior art / parameter source.

## Decisions

| Area | Decision |
| --- | --- |
| Retrieval unit | **Operation-anchored** (root Query + Mutation fields). Exclude Subscriptions; include deprecated ops. |
| Document | **Enriched** (option B): operation name + description + arg names/types + return type name, plus a **bounded downward flatten** of the return type's field names/descriptions. |
| Embedder | **fastembed-rs (ONNX Runtime)**, model `bge-small-en-v1.5` (384-dim). Behind an `Embedder` trait. |
| Vector store | **In-memory brute-force cosine**, pure in-RAM, rebuilt at startup. Behind a `VectorStore` trait. |
| Fusion | **Reciprocal Rank Fusion (RRF)**, `k = 60`. |
| Code structure | **New crate `apollo-schema-search`** for the hybrid layer (Option 2). |
| Result currency | `Scored<OperationRef>` (operation identity + score); no `PathNode` up-walk. |
| Tree-shaking | Stays in the MCP `Search` tool (minimize churn). |
| Result count | New optional `limit` tool param; default **10**, hard cap **50**. |
| ORT linking | **`ort` `load-dynamic`** — bake `libonnxruntime.so` at a fixed path, set `ORT_DYLIB_PATH`, `dlopen` at runtime, catch failure → degrade to BM25. Static-link is the documented fallback. |

## Architecture

Three crates (one new):

### `apollo-schema-index` (existing, refactored) — lexical backend + shared contracts

- Defines the shared **`SchemaSearch`** trait and the **`Scored<OperationRef>`**
  result currency (both live here, the lower crate, to avoid a dependency cycle).
- Enumerates operations (root Query + Mutation fields) and constructs the
  **enriched document** per operation, including the bounded downward return-type
  flatten (depth-limited, cycle-guarded via a visited set).
- Provides the **Tantivy BM25** `SchemaSearch` implementation.
- **Removes** the up-walk machinery (`type_references` graph, `walk_up_to_roots`,
  `build_leaf_path`, `boost_shorter_paths`) and the path-expansion `Options` —
  unnecessary once hits are operations.

### `apollo-schema-search` (new) — hybrid layer

- `Embedder` trait + a **fastembed** implementation (isolates the `ort`
  dependency to this crate).
- `VectorStore` trait + an **in-memory brute-force cosine** implementation
  holding `(OperationRef, Vec<f32>)`.
- A **vector `SchemaSearch`** implementation (embed query → vector top-k).
- **`HybridSearch`**: composes a list of `SchemaSearch` backends and RRF-fuses
  their ranked lists by `OperationRef`.
- Depends on `apollo-schema-index`. This crate ≈ the future extracted service's core.

### `apollo-mcp-server` (consumer)

- The `Search` tool builds a `HybridSearch { lexical, vector }`, calls
  `search(query, limit)`, and tree-shakes the fused top-k into SDL (unchanged
  location).

### Contracts

```
trait SchemaSearch {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError>;
}

trait Embedder {           // in apollo-schema-search
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
}

trait VectorStore {        // in apollo-schema-search
    fn upsert(&mut self, op: OperationRef, vector: Vec<f32>);
    fn search(&self, query_vector: &[f32], limit: usize) -> Vec<Scored<OperationRef>>;
}
```

`OperationRef` identifies an operation (root type + field name); `Scored<T>` is the
existing `{ inner: T, score: f32 }`. Signatures are indicative, not final.

Note on `limit` semantics: at the **backend** level, `limit` is the *candidate pool*
size each backend returns (`HybridSearch` requests ≥ `search.max_limit` from each so
fusion has enough to work with). The user-facing result count (`effective_k`) is
applied by the `Search` tool **after** RRF fusion, not by the individual backends.

### Data flow

- **Build** (startup **and** on `update_schema`): enumerate operations → build one
  enriched document per operation → (a) write to the Tantivy index, (b) `embed()`
  the same document text → `upsert` into the vector store. Same corpus, same unit.
- **Query**: tokenize for BM25 and `embed()` the query → each backend returns a
  ranked `Vec<Scored<OperationRef>>` → **RRF fuse** by `OperationRef` → take
  top-`limit` → tree-shake each operation's return/argument types into SDL.

### Integration & fixes

- **`update_schema` rebuilds both indexes** (closes the staleness bug for BM25 and
  the new vector store).
- All embedding inference (and, while we are here, the existing synchronous Tantivy
  calls) run via **`spawn_blocking`**.

## Retrieval unit & document construction

- **Anchor set:** root `Query` and `Mutation` fields. Subscriptions are excluded
  (not executable as MCP tools). Deprecated operations are included (still callable).
- **Enriched document per operation:** operation name, description, argument names
  and types, return type name, plus a **bounded downward flatten** of the return
  type's field names and descriptions. Depth is configurable
  (`semantic.flatten_depth`, default **1**), and the walk is cycle-guarded with a
  visited set to handle recursive types.
- The same document text feeds both the BM25 tokenizer and the embedder.
- **Behavior change:** operation-anchoring changes recall characteristics vs. the
  field-anchored index (gains clean operation-level hits; loses arbitrary
  deep-field matching beyond the flatten depth). Validate directionally (see below).

## Search tool interface

- New **optional** input parameter `limit` (integer) with a clear description,
  e.g. *"Maximum number of results to return (default 10, max 50)."* Optional, so
  existing callers are unaffected.
- Replace the hardcoded `MAX_SEARCH_RESULTS = 5` with config-driven values:
  - `search.default_limit` (default **10**) — used when `limit` is omitted.
  - `search.max_limit` (default **50**) — the hard cap.
- Behavior: `effective_k = clamp(limit ?? default_limit, 1, max_limit)` — clamp,
  do not error (friendlier for agents; a request for 100 becomes 50, 0 becomes 1).
- Truncation to `effective_k` happens **after** RRF fusion. The per-backend
  candidate pool must be ≥ `max_limit`; Tantivy already pulls 100 candidates and
  the in-memory store returns all sorted, so a cap of 50 is free.

## Configuration

Extend `SearchConfig` (`crates/apollo-mcp-server/src/runtime/introspection.rs`);
all overridable via `APOLLO_MCP_INTROSPECTION__SEARCH__…`:

| Key | Default | Purpose |
| --- | --- | --- |
| `semantic.enabled` | `true` (when search enabled) | Enable semantic backend; **degradable** (see Error handling). |
| `semantic.model` | `bge-small-en-v1.5` | Model name / local path (points at the baked-in model). |
| `semantic.flatten_depth` | `1` | Return-type flatten depth for enriched docs. |
| `semantic.inference_threads` | `1` | Pins ORT `intra_op_num_threads`. |
| `hybrid.rrf_k` | `60` | RRF constant. |
| `search.default_limit` | `10` | Result count when `limit` omitted. |
| `search.max_limit` | `50` | Result-count hard cap. |

## Runtime & concurrency

- The embedder handle lives in an `Arc`; calls are serialized (`Mutex`) or run
  through a small pool. Fine at the expected low QPS / tiny corpus.
- **All inference runs off the async runtime via `spawn_blocking`** so it never
  stalls the tokio executor (which also serves health checks). Wrap the existing
  synchronous Tantivy calls the same way.
- ORT thread pool pinned via `semantic.inference_threads` (default 1) so it
  respects the pod CPU cgroup instead of the host core count.

## Error handling & graceful degradation

- `HybridSearch` composes a list of backends. If the **embedder fails to
  initialize** (missing/corrupt/wrong-path model, ORT load failure), log a warning
  and run **lexical-only**. The MCP server stays up; `search`/`execute`/`introspect`
  keep working.
- A **per-query embed failure** is caught and falls back to the lexical result for
  that query.
- No panics/unwraps/expects (respects the workspace lints). Errors are `Result`s.

## Packaging (Docker)

All changes are in **`apollo-mcp-server`**; `constellation-runtime` only bumps the
`ghcr.io/apollographql/apollo-mcp-server` image tag.

- **Build stage** (`rust:1.92-bookworm`): add `fastembed`/`ort` with the `ort`
  **`load-dynamic`** feature; `ort` fetches the prebuilt ONNX Runtime `.so` at
  build time.
- **Runtime stage** (`gcr.io/distroless/cc-debian12`, glibc + C/C++ runtime):
  - `COPY` the **model file** and **`libonnxruntime.so`** in from the build stage
    to fixed paths (distroless has no shell/apt — everything is baked at build).
  - Use **`load-dynamic`**: set **`ORT_DYLIB_PATH`** to the baked `.so` and
    `dlopen` at runtime. On load failure, **catch and degrade to BM25** (see Error
    handling). This is *why* `load-dynamic` is chosen over a plain dynamic link —
    a NEEDED dependency would let the loader kill the process before our fallback
    code runs, and distroless has no shell to diagnose it.
  - **Disable fastembed's runtime download**; point it at the local model path.
  - Ensure paths are readable by the non-root `USER 1000`.
  - Provision the ORT native lib for **both arches** (amd64 + arm64); model files
    are arch-neutral.
  - *Fallback:* statically linking ORT (single self-contained binary, no external
    `.so`) is the documented alternative if a zero-external-file image is later
    preferred; rejected for now due to from-source build cost, libstdc++
    static-mixing, and doubled multi-arch build time.
- **In `constellation-runtime` (separate, out of scope here, tracked):** raise pod
  memory requests/limits (~150–400 MB for model + ORT arenas; OOMKill risk) and
  relax the readiness/liveness probe timing to accommodate the startup index build.
- **CI / dev:** the `ort` build-time download means first builds need network and
  offline/air-gapped builds break unless ORT is pre-provisioned. Document a CI
  cache strategy.

## Testing & evaluation

**In-repo Rust tests are the gate.** Fast, deterministic, offline:

- **RRF fusion** — pure function: ordering, ties, `k`, single-backend passthrough.
- **`limit` clamping** — omitted / 0 / negative / over-cap / normal.
- **VectorStore** — cosine top-k correctness with hand-picked vectors.
- **Operation enumeration + enriched-doc construction** — schema fixtures assert
  the right anchor set (query+mutation, no subscriptions) and the depth-1 flatten;
  cycle and deprecation handling.
- **Graceful degradation** — inject a failing `Embedder`; assert lexical-only
  results and a logged warning, no panic.
- **Embedder trait with a fake/deterministic impl** for the fast suite, so tests
  are offline and quick (no `ort` download, no real model). One **gated** (feature
  or `#[ignore]`) integration test exercises the real fastembed model.
- **Snapshot tests (`insta`)** on the **ranked operation list (ids/order)**, not
  raw vectors (model-brittle). Adapt existing search snapshots to the
  operation-anchored output.
- 80% patch coverage is achievable — the deterministic components carry most of it.

**Directional validation (not a gate):** use the personal Python PoC as prior art
and, optionally, to sanity-check field-anchored-BM25 vs operation-anchored-BM25 and
BM25-only vs hybrid. It is a quick experiment (n≈1 case-study prompts), not
maintained tooling.

**Optional follow-up:** formalize a small in-repo eval set (prompts → expected
operations) if we want ongoing before/after measurement. Not a blocker for this work.

## Requirements checklist

See the [callouts doc](../notes/2026-07-14-hybrid-search-callouts.md) for the full
list. The load-bearing ones:

- [ ] Inference off the async runtime (`spawn_blocking`); Tantivy calls too.
- [ ] ORT `intra_op_num_threads` pinned; verified in a CPU-limited pod.
- [ ] Embedder init failure degrades to BM25-only (no crash) + logs.
- [ ] `update_schema` rebuilds both indexes.
- [ ] Search behind trait seam (`SchemaSearch` / `Embedder` / `VectorStore` +
      `HybridSearch`).
- [ ] Model + ORT lib baked into the image; runtime download disabled;
      `ORT_DYLIB_PATH` set; multi-arch; readable by `USER 1000`.
- [ ] `limit` param (default 10, cap 50), clamped.
- [ ] Pod memory limits + readiness probe timing raised in `constellation-runtime`.
- [ ] CI/offline build strategy for the `ort` download documented.

## Open questions

- **Model license** — confirm the chosen model's license (bge = MIT; ONNX
  Runtime = MIT).
- **Persistence** — deferred; pure in-RAM for now. Revisit if startup embedding
  time becomes noticeable at larger corpora.

## Future / extraction

The trait seam (`SchemaSearch` / `Embedder` / `VectorStore` + `HybridSearch`) and
the dedicated `apollo-schema-search` crate are the extraction boundary. The future
standalone service lifts this crate and puts an API in front of it, and can swap
the in-memory `VectorStore` for a **Qdrant-backed** implementation and/or the
fastembed `Embedder` for a candle-based one — without touching the consumers.
