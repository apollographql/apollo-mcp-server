# AIR-311 — Hybrid Search: Callouts & Pitfalls

**Status:** working notes / tracking doc (design phase — not a spec)
**Branch:** `air-311-hybrid-search` (off `tninesling/field-search`)
**Last updated:** 2026-07-14

Living checklist of the operational concerns, risks, and derived requirements for
adding **short-term hybrid (BM25 + semantic) search** to the Apollo MCP Server.
This is not the design spec; it's the running list of things that will bite us if
unhandled. Update as decisions land.

---

## Goal & scope

Add a **short-term, in-process** semantic search alongside the existing BM25
lexical search, fused into hybrid results. Constraints:

- Must be **simple to stand up** and **easy to run on-premise** (incl. air-gapped).
- Explicitly a **short-term** solution — search will eventually be extracted into
  its own service. Work should be **partially liftable** to that service, not
  throwaway.
- Runs inside the existing `apollo-mcp-server` binary (distroless image), deployed
  by `constellation-runtime` as a **separate container** (helm just pins the
  `ghcr.io/apollographql/apollo-mcp-server` image tag).

## Current-state facts (verified in-repo)

- **BM25 = Tantivy `0.24.2`**, index built **in-RAM** at startup in
  `crates/apollo-schema-index` (`SchemaIndex::new` → `Index::create_in_ram`).
- Index is **field-anchored** today (one doc per object/interface field + enum
  value), with a type-reference graph used to walk hits *up* to an operation.
- **No re-index on schema hot-reload** — `Running::update_schema` swaps the schema
  but never rebuilds the index, so **BM25 goes stale after a reload**
  (pre-existing bug; see below).
- **No search abstraction** — Tantivy is hardwired end-to-end. Shared result
  currency is `Vec<Scored<PathNode>>`; post-retrieval path-walking / tree-shaking
  is backend-agnostic.
- **No ML/vector/embedding deps** anywhere in the workspace today.
- Constraints: edition 2024 / Rust 1.92, `deny(unwrap/expect/panic/indexing_slicing)`,
  80% patch coverage. Tantivy currently runs **synchronously inside async handlers**.

## Decisions so far (design phase)

- **Retrieval unit → operation-anchored, enriched documents (option B).** Anchor
  documents on root Query/Mutation fields (operations), and fold a *bounded flatten*
  of the return type's field names + descriptions into each operation's document.
  Applies to **both** BM25 and semantic so fusion is over the same unit.
- **Embedder → fastembed-rs (ONNX Runtime), option A.** Fastest to stand up; the
  distroless/glibc base makes ORT viable (see packaging). Behind an `Embedder`
  trait so it can be swapped (candle) or lifted to the extracted service.
- **Vector store → in-memory brute-force cosine** (matches the in-RAM BM25 model;
  corpus is tiny). Behind a `VectorStore` trait; **Qdrant is the extraction
  target, not a short-term dependency.**
- **Fusion → Reciprocal Rank Fusion (RRF).** Rank-based, no score normalization.
- **Extraction seam → traits** (`Embedder` / `VectorStore` / a `SchemaSearch`
  backend + fusion layer). The trait is what later lifts into the standalone service.

---

## Callouts & pitfalls

### A. Must-handle runtime requirements

1. **Do not block the async runtime.** Embedding inference is CPU-bound and
   synchronous. Running it on tokio's async threads stalls the executor (other MCP
   requests + health checks hang). **Run all inference via `spawn_blocking` or a
   dedicated pool.** (Same applies to the existing sync Tantivy calls under load.)
2. **Cap ORT threads.** ONNX Runtime defaults its intra-op thread pool to the
   *host's* core count, ignoring the pod CPU cgroup → thread over-subscription and
   latency thrash. **Pin `intra_op_num_threads` (1–2).** fastembed exposes limited
   knobs; may need ORT session options / env.
3. **Raise pod memory limits (OOMKill risk).** ORT session + model weights + ORT
   arenas add ~**150–400 MB** RSS. If the helm memory limit isn't bumped, k8s
   **OOMKills** the container. Bump requests/limits when this ships.
4. **Graceful degradation on embedder init failure.** A missing/corrupt/wrong-path
   model or ORT init failure must **fall back to BM25-only + log loudly**, not take
   down the whole MCP server (which also serves `execute`/`introspect`).

### B. Startup & health probes

5. **Longer startup.** Embedding the whole catalog at boot adds CPU-bound work
   (seconds). **Readiness/liveness probe timing must tolerate it** or k8s kills the
   pod mid-index-build in a restart loop. Check `initialDelaySeconds`/timeouts.

### C. Crash-specific risks (lower probability)

6. **Native segfault/abort in ORT bypasses Rust panic handling** — can't be caught/
   unwound. Low risk with a fixed small model + controlled short inputs, but it's a
   different failure mode than the `deny(panic)` Rust code.
7. **Dynamic-link failure at first use.** If ORT is dynamically linked and
   `libonnxruntime.so` isn't findable at runtime (distroless has **no shell** to
   debug), the first embed call crashes. Bake the `.so` at a known path + set
   `ORT_DYLIB_PATH` (ort `load-dynamic`), or static-link.
8. **glibc/libstdc++ mismatch** against `distroless/cc-debian12` is a theoretical
   crash-at-load; low risk since ORT prebuilts target older glibc and the base is
   glibc (not musl).

### D. Dev & build impact (friction, not runtime)

9. **`ort` downloads a prebuilt ONNX Runtime at build time.** Every contributor's
   first `cargo build` and CI now needs network for it; builds get bigger/slower;
   **fully offline/air-gapped builds break** unless ORT is pre-provisioned. Needs a
   CI cache strategy + a heads-up to the team.
10. **Concurrency model.** fastembed's `TextEmbedding` handle is shared across
    requests (`Arc`, likely serialized via `Mutex` or a tiny pool). Fine at low
    QPS / tiny corpus, but it's a real design detail.

### E. Packaging (distroless/cc-debian12)

11. **Base is glibc + the `cc` variant** — ships the C/C++ runtime (glibc, libgcc,
    libstdc++) ORT needs. Good fit. **But distroless has no shell / no apt**, so
    **everything must be baked at build time via `COPY`** — no runtime model
    download, no `apt install`. (This is the air-gapped-friendly pattern anyway.)
12. **Bake the model + ORT lib at build; disable fastembed's runtime HF fetch.**
    Point fastembed at the local model path; ensure paths are readable by the
    non-root `USER 1000`.
13. **Multi-arch build (amd64 + arm64).** Model files are arch-neutral; the ORT
    native lib is per-arch (`ort` handles it — verify both).
14. **Image size** grows by ~model (bge-small ≈ 30–130 MB) + ORT lib (~15–40 MB if
    dynamic). Slower pulls / cold start.
15. **Work lands in `apollo-mcp-server`, not `constellation-runtime`.** Code + the
    `apollo-mcp-server/Dockerfile` change happen here; `constellation-runtime` only
    bumps the image tag.

### F. Cross-cutting

16. **Model-version invariant.** Query and index embeddings **must** come from the
    same model + version, or similarity is meaningless. Index is rebuilt at startup
    from the baked model (self-consistent per image); tag the index with model id
    if we ever persist it. Changing the model = full re-embed.
17. **Fix the hot-reload staleness gap.** `Running::update_schema` must rebuild the
    index (both BM25 and the new vector store) on schema change. Pre-existing bug
    for BM25 alone; the hybrid work should close it.
18. **Behavior change to existing BM25.** Operation-anchoring changes recall
    characteristics (gain clean operation-level hits; lose arbitrary deep-field
    matching beyond the flatten depth). **Validate against the multi-intent eval set
    before/after.**
19. **New knobs:** return-type flatten depth (default 1–2, guard cycles with a
    visited-set), embedding dimensionality (short-term: a 384-dim small model like
    bge-small is fine), RRF `k` (default 60).
20. **Licenses:** ONNX Runtime = MIT; bge = MIT, MiniLM = Apache-2.0 — confirm the
    chosen model's license.

---

## Derived requirements checklist

- [ ] All embedding inference runs off the async runtime (`spawn_blocking`/pool).
- [ ] ORT `intra_op_num_threads` pinned (1–2), verified inside a CPU-limited pod.
- [ ] Pod memory requests/limits raised in helm; verified no OOMKill under load.
- [ ] Embedder init failure degrades to BM25-only (no process crash) + logs.
- [ ] Readiness/liveness probe timing accommodates startup index build.
- [ ] Model + ORT lib baked into the image at build; runtime HF download disabled.
- [ ] `ORT_DYLIB_PATH` set (or ORT static-linked); paths readable by `USER 1000`.
- [ ] Multi-arch (amd64/arm64) ORT lib provisioned and tested.
- [ ] CI/offline build strategy for the `ort` build-time download documented.
- [ ] `update_schema` rebuilds both BM25 and vector indexes on hot-reload.
- [ ] Search behind trait seam (`Embedder`/`VectorStore`/`SchemaSearch` + fusion).
- [ ] Before/after eval on the multi-intent set for the operation-anchoring change.
- [ ] Model license confirmed.

## Open questions / to decide

- Include subscriptions / deprecated operations in the operation anchor set?
- Static-link ORT vs. `COPY` the `.so` + `ORT_DYLIB_PATH`?
- Embedding model + dimensionality for the short term (bge-small 384 vs 768)?
- Return-type flatten depth default (1 vs 2)?
- Persist the index to disk at all, or keep purely in-RAM (rebuild at startup)?
