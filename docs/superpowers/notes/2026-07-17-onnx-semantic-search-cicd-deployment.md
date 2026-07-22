# Shipping ONNX-based semantic search through CI/CD (GitHub Actions → GHCR → ArgoCD/k8s)

**Status:** deployment runbook / checklist
**Date:** 2026-07-17
**Scope:** what it takes to ship the hybrid (semantic) search feature — which currently uses `fastembed` → `ort` → ONNX Runtime — through the real pipeline: build in **GitHub Actions** (`apollo-mcp-server`), publish to **GHCR**, deploy via **ArgoCD** using the **`constellation-runtime`** Helm chart on **Kubernetes**. Derived from getting the identical setup running in the `search-benchmark` Docker harness.

> **Assumptions:** the build pipeline (GitHub Actions runners) **has network egress**. The *deployment* (k8s pods) may or may not — treat it as if it does **not** reach HuggingFace.

---

## 0. Why anything is needed (what the benchmark proved)

The stock image does not build or run semantic search as-is. Getting it working in Docker required, in order:
1. A newer base image (the ONNX Runtime prebuilt won't link against the current bookworm/glibc-2.36 base).
2. Baking the embedding model into the image (the runtime HuggingFace download fails / can't be relied on).
3. Enough container memory (ONNX model + arenas + embedding activations exceed the current 512 Mi limit).
4. Tolerating a slow first boot (~140 s to embed the corpus) — mitigated by a shared Postgres embedding cache reused across restarts and replicas.

Each maps to concrete changes below.

---

## Part A — `apollo-mcp-server` repo: `Dockerfile`

The build- and run-stage changes. (Benchmark reference: `search-benchmark/docker/Dockerfile.hybrid` is the proven working recipe.)

- [ ] **A1. Bump both base images off Debian 12.** `FROM rust:1.92.0-bookworm` → `rust:1.92.0-trixie` (build), and `gcr.io/distroless/cc-debian12` → a Debian-13 runtime base. **Required** — without it the build fails to link ONNX Runtime (`undefined reference to __isoc23_strtoull` / `__cxa_call_terminate`, i.e. glibc 2.38+/newer libstdc++). Decide the runtime base:
  - `gcr.io/distroless/cc-debian13` **if published** (keeps distroless; bundles `libstdc++`/`libgcc`), **or**
  - `debian:trixie-slim` + `apt-get install -y ca-certificates libssl3 libstdc++6 libgcc-s1` (drops distroless minimalism; must install ORT's runtime libs yourself).
- [ ] **A2. Bake the embedding model into the image.**
  - **Why:** on the first search, the server downloads the embedding model (`Xenova/bge-small-en-v1.5`, ~133 MB) from HuggingFace. If the running pod can't reach HuggingFace — common for on-prem / locked-down deployments — that download fails and search silently drops to lexical-only. Baking the model into the image removes the runtime download entirely.
  - **How** (all in the Dockerfile; the *build* has network, the *pod* need not):
    1. In the **builder stage**, get the model into fastembed's cache directory `./.fastembed_cache/` — pick **one** of the three options below.
    2. `COPY` that `.fastembed_cache/` from the builder stage into the runtime image, at the server's working directory, owned by `USER 1000`.
    3. Set `ENV HF_HUB_OFFLINE=1` so the server reads the baked copy and never attempts a network fetch.
  - **Options for step 1 (pick one):**

    **Option 1 — HuggingFace CLI (recommended; the standard way, no code):**
    ```dockerfile
    RUN apt-get update && apt-get install -y python3-pip && pip3 install -q huggingface_hub
    RUN HF_HUB_CACHE=/app/.fastembed_cache huggingface-cli download Xenova/bge-small-en-v1.5
    ```
    Pros: the normal HF tooling; nothing to write. Watch: `HF_HUB_CACHE` must point at fastembed's cache dir so the `models--…` tree lands where fastembed reads it; verify with the offline smoke test (Part D). Adds Python to the *build* stage only.

    **Option 2 — Warm-up example (layout guaranteed by construction):** add a small build-only example — it doesn't exist yet; `FastembedEmbedder::new` does and is public. Create `crates/apollo-schema-search/examples/warm_embed_cache.rs`:
    ```rust
    // Build-time helper: constructing the embedder makes fastembed download the
    // model into ./.fastembed_cache so it can be baked into the image.
    fn main() -> Result<(), Box<dyn std::error::Error>> {
        apollo_schema_search::FastembedEmbedder::new("bge-small-en-v1.5", 1)?;
        Ok(())
    }
    ```
    then `RUN cargo run --release -p apollo-schema-search --example warm_embed_cache`. Pros: fastembed writes the exact layout it later reads; doubles as a build-time check that ONNX Runtime loads. Cons: you add/maintain the example; ORT must run at build. (Return `Result`, not `.unwrap()`, to stay clippy-clean under the repo's deny-lints.)

    **Option 3 — Vendor the files and `COPY` them (no build-time network):** obtain the model files once (`model.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json`) — e.g. via `curl` from `https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/…` or committed to an artifact store — and `COPY` them into `.fastembed_cache/` in the hub layout (`models--Xenova--bge-small-en-v1.5/{blobs,snapshots,refs}`). Pros: reproducible, air-gap-friendly *build*. Cons: you version/store a ~133 MB blob **and** must reproduce the cache layout by hand (blobs are content-hashed, snapshots symlink to them) — the fiddliest option.

    *(Separate, cleaner long-term but a code change, not a build step: switch the server to fastembed's user-defined-model API (`try_new_from_user_defined`) so it loads flat `model.onnx`/`tokenizer.json` files you just `COPY` — dropping the hub-cache-layout concern entirely.)*
- [ ] **A3. Give `USER 1000` a writable home directory.** In a distroless/minimal image, the non-root user (uid 1000) often has no writable `$HOME`. The HuggingFace libraries underneath fastembed may try to write a small lock/index file under `$HOME` at startup — even in offline mode — and that write *fails* if `$HOME` isn't writable, which can abort embedder init. Setting `ENV HOME=/tmp` (world-writable in every base image) makes that write succeed.
  - This is **separate from the model cache in A2**: the model lives in the baked `.fastembed_cache` (read at startup); A3 just prevents an *unrelated* HuggingFace-library write to `$HOME` from erroring.
  - Defensive: it was set in the benchmark image and worked; if a build proves it's not needed, drop it.
- [ ] **A4. (Recommended, not required to ship) Plan for `load-dynamic`.** Static `download-binaries` couples the build to the base image's glibc forever. The design's Phase-3 `load-dynamic` (bake `libonnxruntime.so`, `dlopen` at runtime, degrade on load failure) removes that coupling and makes failures graceful. Not needed for a first release; note it as the hardening path.

Image size grows ~150 MB (model ~133 MB + ORT lib) → slower pulls / cold starts.

---

## Part B — `apollo-mcp-server` GitHub Actions

The good news: **multi-arch is already handled.** `.github/workflows/release-container.yml` builds per-arch on native runners (matrix `linux/amd64` + `linux/arm64`, `docker/build-push-action@v6`, `context: .`, `file: ./Dockerfile`), pushes `:VERSION-<arch>` tags, then a manifest job stitches them into a multi-arch tag. `release-canary.yml` computes `canary-<DATE>-<SHA>` and reuses that flow (this is how the `canary-…989f511` image was produced).

- [ ] **B1. No workflow change needed for multi-arch** — the per-arch native-runner matrix already pulls the correct per-arch ONNX Runtime prebuilt during `cargo build`. Verify the trixie-based `Dockerfile` builds cleanly on **both** the amd64 and arm64 runners.
- [ ] **B2. Nothing to run — just keep the build networked.** Two downloads happen *automatically during `cargo build`*, neither of which is a command you invoke:
  - **The ONNX Runtime engine.** `fastembed` depends on the `ort` crate, whose **`download-binaries` feature is on by default**. That feature makes `ort`'s build script fetch the prebuilt ONNX Runtime C++ library (from a CDN) and link it in — as part of normal compilation, on whatever runs `cargo build`.
  - **The model** (A2's step) — the bge-small weights.
  - Both just need internet on the GitHub runner, which it has. So the only "requirement" here is a non-action: **don't** run these builds on an air-gapped runner. (This is distinct from A2: B2 fetches the *engine*, A2 fetches the *model*.) If `ort` were ever switched to the `load-dynamic` feature, this engine download would disappear — nothing fetched at build; you'd bake `libonnxruntime.so` and load it at runtime instead.
- [ ] **B3. Watch build time / caching.** Compiling the ORT-linked binary + downloading ORT and the model lengthens the build. Cache cargo + the ORT download between runs if build time regresses.
- [ ] **B4. Add a smoke-test job (recommended gate).** After build, run the image with a tiny local schema and **assert semantic initialized from the baked model with no network** (i.e. logs `Embedded corpus in …` / `Loaded all embeddings from cache`, **not** `semantic search disabled (lexical-only)`). Fails the release if the model bake or ORT lib is broken. Run it with `--network=none` to prove offline-readiness.
- [ ] **B5. Publish + tag** — unchanged (existing container/canary workflow). Output is the multi-arch `ghcr.io/apollographql/apollo-mcp-server:<tag>`.

---

## Part C — `constellation-runtime` (Helm chart, deployed by ArgoCD)

The runtime pod is a **`Deployment`** (`templates/deployment.yaml`, `replicas: {{ .Values.replicas }}` = 2) with **router + mcp + mcp-proxy co-located in one pod**. The chart only *consumes* the mcp image by tag; it never builds it.

- [ ] **C1. Bump the image tag.** `values.yaml` → `mcp.image.tag` → the new multi-arch tag from Part B.
- [ ] **C2. Raise the mcp memory limit.** `mcp.resources.limits.memory` is **512 Mi** today — too small for ONNX (model + ORT arenas + embedding activations). Raise it (start ~**2 Gi**; measured hybrid peak was ~2.5 GB during a bounded-batch embed, and grows with corpus/dimensionality). Also bump `requests.memory` accordingly.
- [ ] **C3. Give the mcp container a boot-tolerant probe.** The first cold embed is ~140 s (longer on bigger schemas). Ensure the mcp container has a **`startupProbe`** with a `failureThreshold × periodSeconds` window that comfortably exceeds the cold-embed time (mirror the router's `startupProbe` pattern already in `deployment.yaml`), so k8s doesn't kill the pod mid-embed on first boot. Verify the mcp readiness probe isn't tripping during embed.
- [ ] **C4. Enable semantic + point at the cache in config.** In `mcp.configuration.introspection.search` add:
  ```yaml
  semantic:
    enabled: true
    model: bge-small-en-v1.5
    inference_threads: 2            # pin small; do NOT default to host core count in a CPU-limited pod
    cache_url: ${env.EMBEDDING_CACHE_DATABASE_URL}   # Postgres connection string (see C5)
  ```
  (Config is delivered via the chart's ConfigMap → the mcp container's config file. `cache_url` unset = no cache = embed every start, fail-open. The URL's password comes from a Secret via `${env.…}` expansion — never inlined.)
- [ ] **C5. Deploy the shared embedding cache as Postgres (`embedding-db`).** The cache only helps *across restarts* and must outlive the pod. Because the runtime is a multi-replica `Deployment` (can't own a per-replica `volumeClaimTemplates` PVC, and a single `ReadWriteOnce` PVC can't attach to 2 pods), store the cache in **Postgres** rather than a node-local file: add a dedicated single-replica `embedding-db` `StatefulSet` (its own PVC + headless Service, mirroring `keycloak-db`) and point every mcp replica at it via `cache_url` (C4). All replicas share one durable cache — the first to boot on a new schema embeds and writes, the rest read. A managed Postgres (e.g. CloudSQL) is a drop-in alternative; the mcp server only needs a connection URL. Embeddings are content+generation-keyed and idempotently written (`ON CONFLICT DO NOTHING`), so concurrent writers are safe.
  - **Why not just bake the embeddings into the image, the way A2 bakes the model?** Because they're two different artifacts, and only one is knowable at build time:
    - The **model** (`.fastembed_cache`, the bge-small weights) is *identical for every deployment* and fixed at build → baked into the image (A2). ✅
    - The **embeddings** are the *output of running that model over a specific supergraph's operations*. The supergraph is fetched from **uplink at runtime** (managed federation) and differs per environment/graph/customer — so at build time there is **no schema to embed**, hence nothing to bake. ❌
    - They're re-derived whenever a new supergraph launches (the content-addressed cache re-embeds just the changed ops). A baked snapshot would be both wrong-for-this-deployment and stale after the next schema change — hence a *runtime* shared store (Postgres), not a build-time artifact.
- [ ] **C6. Add the `embedding-db` resources.** `embedding-db.yaml`: single-replica Postgres `StatefulSet` (own `volumeClaimTemplates` RWO PVC at `/var/lib/postgresql/data`, ~1 Gi) + headless Service + password Secret, gated on `embeddingCache.enabled`. On the mcp container add `EMBEDDING_CACHE_DATABASE_URL` (password from the Secret) and the `cache_url` config (C4). The runtime `Deployment` itself needs **no** volume or PVC.
- [ ] **C7. Helm lint / template / kind smoke** per the repo's CONTRIBUTING (the `helm lint` + `helm template` + kind loop) before the ArgoCD sync.
- [ ] **C8. ArgoCD:** the promotion of `mcp.image.tag` (and the values above) flows through apollo-argo's `application-values.yaml` as usual; no ArgoCD-specific change beyond the values.

---

## Part D — Verification checklist (per environment)

- [ ] Image builds on both amd64 and arm64 runners (Part B).
- [ ] `docker run --network=none <image>` starts, logs the corpus-embed (or cache-load) line, and does **not** log `semantic search disabled (lexical-only)` (proves the baked model works offline).
- [ ] In-cluster: the mcp pod reaches Ready (startup probe tolerated the cold embed); memory stays under the new limit (no OOMKill).
- [ ] `search` returns results and the tool is `mcp__apollo__search` (semantic active).
- [ ] After a pod restart/redeploy, the second boot is fast (Postgres cache reused) — confirm via the `reused=<N>` / "Loaded all embeddings from cache" log line.

---

## Open decisions / residual risks

- **Base bump vs `load-dynamic`** (A1 vs A4): base bump ships fastest; `load-dynamic` is the robust long-term fix (decouples ORT from the base, graceful degrade). Recommend ship on base bump, harden with `load-dynamic` later.
- **Cache persistence** (C5): resolved — store the cache in a Postgres `embedding-db` StatefulSet (or managed Postgres) shared by all replicas; the runtime `Deployment` needs no PVC. Cross-replica cold-start serialization (a session advisory lock, or `maxSurge: 1`) is a later refinement — concurrent cold embeds are correct today, just redundant.
- **Native crash mode:** an ONNX Runtime segfault bypasses Rust panic safety (graceful-degrade covers init failure, not a mid-inference crash). Low probability with a fixed small model; note for monitoring.
- **Bigger-picture alternative:** if the ONNX native dependency is judged too heavy for on-prem customers, a pure-Rust embedder (`tract` reusing the same `.onnx`, or Candle, or Model2Vec) behind the existing `Embedder` trait would remove Parts A2/A4 and the base-bump entirely. Separate spike; see the hybrid-search design notes.

---

## Appendix — the proven benchmark recipe (template for A1–A3)

`search-benchmark/docker/Dockerfile.hybrid` (trixie base + baked model + runtime libs) is the working reference. Differences for production: fetch the model **during the networked build** instead of `COPY`ing a pre-existing host `.fastembed_cache`, and use the repo's real `Dockerfile` (all crates) rather than the benchmark's slimmed copy.
