# Design — Externalize the embedding cache to Postgres

**Date:** 2026-07-21 · **Branch (base):** `air-311-hybrid-search` · **Status:** design approved, pending spec review
**Relates to:** [SQLite embedding-cache plan](../plans/2026-07-17-embedding-cache-sqlite.md), [ONNX/semantic CI-CD runbook](../notes/2026-07-17-onnx-semantic-search-cicd-deployment.md), [session handoff](../notes/2026-07-21-session-handoff-hybrid-search.md)

> **Amendment (2026-07-21, during implementation):** SQLite support was dropped
> entirely — **Postgres is the only backend**. This collapses the config from a
> tagged `CacheConfig` enum to a single optional field
> `introspection.search.semantic.cache_url: Option<String>` (a Postgres
> connection URL; unset = caching disabled). The `EmbeddingStore` trait is
> retained — its in-memory test double keeps the incremental-reuse logic covered
> in the offline test suite. Every other decision below — generation-keyed
> exact-tuple schema, fail-open, multi-writer idempotency, the `embedding-db`
> StatefulSet — stands as written, substituting `cache_url` for the SQLite/enum
> config surface.

## Summary

The semantic-search embedding cache is currently a **content-addressed SQLite file** (`apollo-schema-search/src/embedding_cache.rs`). It works, but persisting the file across pod restarts is the unsolved deploy problem ("C5" in the CI/CD runbook): the runtime workload is a **2-replica `Deployment`**, which cannot use per-replica `volumeClaimTemplates` (a StatefulSet feature) and cannot share a single `ReadWriteOnce` PVC across pods.

This design **externalizes the cache to Postgres**, deployed as a dedicated single-replica `StatefulSet` that mirrors the existing `keycloak-db` precedent in the `constellation-runtime` Helm chart. The stateful storage lives on the StatefulSet's own PVC; all MCP replicas share the cache over the DB's headless Service. This:

- removes the per-pod PVC requirement from the runtime `Deployment` entirely;
- makes the cache durable across pod lifecycle and shareable across replicas — the first replica to boot on a new schema embeds and writes, the rest read;
- lets the datastore scale/operate independently of the MCP server;
- reuses infrastructure and operational patterns the team already runs (Keycloak's Postgres StatefulSet).

**Scope: cache only ("Role A").** The datastore stores precomputed embeddings; the MCP server continues to load vectors into memory and perform exact in-memory brute-force cosine search. Offloading vector *search* into a vector DB (Qdrant/pgvector) is explicitly **out of scope** — see Non-Goals.

## Goals

- Embeddings persist across restarts and are shared by all MCP replicas, so the ~140 s ONNX embed is paid once per (embedding-semantics, operation), not once per pod per restart.
- No stateful storage on the multi-replica runtime `Deployment`.
- The embedding datastore is deployed and scaled independently of the MCP server.
- Preserve the existing cache guarantees verbatim: content-addressed incremental reuse, fail-open, and correct invalidation when embedding semantics change.
- SQLite remains a supported backend (local dev, single-node, tests).

## Non-Goals

- **Offloading vector search into the DB** (Qdrant / pgvector ANN). At the current corpus size (hundreds to low-thousands of operations; ~1.5 KB per 384-dim vector; single-digit MB total), in-memory brute-force cosine is already exact and sub-millisecond. ANN indexes solve approximate search at scale — a problem this corpus does not have. Revisit only if the corpus grows to ~100k+ operations or per-replica vector memory becomes a concern.
- **Having the datastore compute embeddings.** Self-hosted Qdrant/Postgres do not embed; the ONNX embed stays in the MCP server regardless of datastore. This design does not change where embedding happens.
- **Removing the cold-start embed.** A shared store removes the *repetition* of embedding, not the first embed. The `startupProbe` tolerating the cold embed (runbook C3) still applies.
- Advisory-lock-based single-embedder coordination (deferred; see Coordination).

## Background: what exists today

- `EmbeddingCache` (concrete rusqlite struct): `open` (with a `meta` guard), `get`, `put_batch`; free fn `doc_key`; consts `DOC_BUILDER_VERSION`, `VECTOR_DTYPE`.
- Content key: `op_key = hex(SHA-256(OperationDocument.text))` — changed op → miss → re-embed; unchanged op → reuse. Stable across schema sources.
- Global invalidation via a one-row `meta` guard comparing `(model_id, dim, dtype, doc_builder_ver)`; on mismatch it `DELETE FROM embeddings` and rewrites meta.
- Vectors serialized as raw little-endian f32 (`dim×4` bytes; `dtype = "f32le"`).
- `VectorSearch::build(..., cache: Option<&mut EmbeddingCache>)`: pass 1 reuses cache hits, pass 2 embeds only misses and persists them.
- Consumed in `apollo-mcp-server/src/introspection/tools/search.rs::build_backend`, which opens the cache and passes it to `VectorSearch::build`. `Search` retains `cache_path` so `rebuild` can reopen fail-open.

## Design

### 1. `EmbeddingStore` trait (backend abstraction)

Extract the cache behind a trait so the backend is swappable:

```rust
pub trait EmbeddingStore: Send {
    /// Fetch a cached vector by content key. `Ok(None)` = miss.
    fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, CacheError>;
    /// Insert/replace `(key, vector)` pairs idempotently.
    fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError>;
}
```

- `&mut self` on both methods: the synchronous `postgres::Client` requires `&mut` to query; SQLite tolerates it.
- Two impls:
  - `SqliteCache` — today's struct, renamed; behavior unchanged (keeps its `meta`-guard model).
  - `PostgresCache` — new (see §2–§3).
- Shared free functions (no duplication): `doc_key`, `vec_to_blob`, `blob_to_vec` (with its length validation), `DOC_BUILDER_VERSION`.
- `VectorSearch::build` changes its parameter from `Option<&mut EmbeddingCache>` to `Option<&mut dyn EmbeddingStore>`. The two-pass reuse/embed logic is otherwise untouched — it only calls `get`/`put_batch`.

### 2. Postgres data model — generation-keyed (exact-tuple composite key)

The Postgres cache is shared by all replicas, unlike the single-writer SQLite file, so the SQLite `meta`-guard's destructive `DELETE FROM embeddings` on mismatch is unsafe here: during a rollout that changes the embedding model or doc format, an old-model pod and a new-model pod would each delete the other's vectors and re-embed, ping-ponging until the rollout completes.

Instead, **partition rows by embedding-semantics generation using the exact tuple as part of the primary key** — no destructive delete, no cross-writer contention:

```sql
CREATE TABLE IF NOT EXISTS embeddings (
  model_id        text    NOT NULL,
  dim             integer NOT NULL,
  dtype           text    NOT NULL,
  doc_builder_ver integer NOT NULL,
  op_key          text    NOT NULL,   -- hex(SHA-256(doc text))
  vector          bytea   NOT NULL,   -- raw little-endian f32, dim*4 bytes
  PRIMARY KEY (model_id, dim, dtype, doc_builder_ver, op_key)
);
```

- **No hashing on the generation axis.** The generation is the exact `(model_id, dim, dtype, doc_builder_ver)` tuple, so generation collisions are impossible by construction. (Rationale: a *truncated* generation hash could collide two same-dimension models and silently serve the wrong vector; an exact composite key eliminates that class entirely. A single-column full-SHA-256 generation would also be collision-safe but needlessly hashes four low-cardinality fields.)
- `op_key` remains a **full SHA-256** — identical to today's SQLite/2a design; collision probability is cryptographically negligible and this design adds no new op_key risk.
- Reads/writes always scope to the *current* tuple; rows from other generations are inert (harmless), and prunable out-of-band (`DELETE WHERE model_id = '<old>'`) without racing live writers.
- Vector serialization is unchanged (`vec_to_blob`/`blob_to_vec`), so `bytea` is a dumb blob — no `pgvector` extension, stock `postgres` image suffices.

**Defense-in-depth (already present):** `blob_to_vec` rejects any blob whose length ≠ `dim×4`, and `build` treats a `get` error as a miss and re-embeds. So any dimension mismatch fails safe; the exact-tuple key closes the only remaining (same-dim, different-model) window.

### 3. `PostgresCache` behavior

- **Client:** the synchronous `postgres` crate, keeping `VectorSearch::build` synchronous (no async refactor of the build path). In-cluster connections are plaintext by default, mirroring `keycloak-db`. TLS is a configurable add-on (deferred unless required).
- **`open`/connect:** connect using the configured URL; `CREATE TABLE IF NOT EXISTS` (idempotent, safe for concurrent replicas). Store the current generation tuple on the struct alongside `dim`. Failure to connect → caller falls back to no cache (fail-open, §5).
- **`get(key)`:** `SELECT vector FROM embeddings WHERE model_id=$1 AND dim=$2 AND dtype=$3 AND doc_builder_ver=$4 AND op_key=$5`; deserialize + length-validate via `blob_to_vec`.
- **`put_batch(entries)`:** one transaction; per-entry length validated against `dim` before insert (mismatch → `CacheError::BadVectorLen`, transaction dropped uncommitted → rollback, no partial write); `INSERT ... ON CONFLICT (model_id, dim, dtype, doc_builder_ver, op_key) DO NOTHING` so concurrent writers are idempotent. Commit only after all inserts.

### 4. Configuration surface

Phase 2 has not shipped, so `semantic.cache_path` can be restructured without a compatibility burden. Replace it with a tagged-enum `cache` block under `introspection.search.semantic` (keeping `serde(default, deny_unknown_fields)`):

```yaml
introspection:
  search:
    semantic:
      # omit `cache` entirely  => no cache (embed every start), unchanged default
      cache:
        type: postgres                                   # or: sqlite
        url: ${env.EMBEDDING_CACHE_DATABASE_URL}         # postgres
        # path: /cache/embeddings.db                     # sqlite (mutually exclusive)
```

- The Postgres URL is sourced from an env var backed by a k8s Secret; the password is never inlined (mirrors Keycloak's `dbPassword`). YAML `${env.VAR}` expansion already supports this.
- `Search` retains the resolved cache config (replacing the current `cache_path` field) so `rebuild` reopens the store fail-open across schema reloads.
- `build_backend` constructs the configured store (`Box<dyn EmbeddingStore>` or `None`) instead of calling `EmbeddingCache::open` directly.

### 5. Error handling — fail-open preserved

The existing three-layer fail-open is kept verbatim and extended to Postgres:

1. **Open/connect failure** → run with no cache (embed in-memory from scratch, warn). Server always comes up.
2. **`get` error** → treated as a miss (re-embed that op).
3. **`put_batch` error** → log + continue (results still served; just not persisted this round).

`CacheError` gains a backend-error variant (`#[from] postgres::Error` or a `Backend(String)`); `BadVectorLen` is unchanged. A DB outage never degrades below today's lexical+in-memory-semantic behavior.

### 6. Multi-writer coordination

Writes are idempotent (`ON CONFLICT DO NOTHING`), so concurrent writers are always safe — no corruption; at worst two replicas embed the same miss once.

- **Rolling update (the common case):** pods restart one at a time. The first new pod embeds + writes; later pods read the now-warm shared cache. This matches the "only one instance pays cold-start" expectation.
- **Full-fleet cold start** (brand-new schema generation, or all pods down): several replicas may embed concurrently — correct, bounded, self-healing (next restart is warm).
- **Decision (for now): rely on rolling-update serialization only.** Set the runtime Deployment's rollout to one-at-a-time (`maxSurge: 1`) so scenario 1 (startup/rollout) has a single cold-embedder that warms the shared cache before the next pod starts. This is a deploy-side (Helm) setting — no application code — and is tracked in the follow-up deploy plan.
- **Not handled by the above:** a *runtime* schema change (uplink pushes a new supergraph to already-running replicas) makes all live replicas `rebuild` concurrently; k8s ordering cannot serialize that. Accepted for now (idempotent writes keep it correct, just redundant).
- **Advisory locks deferred.** A session-scoped Postgres advisory lock keyed by the generation tuple would make exactly one replica embed in *both* scenarios (session scope auto-releases on a crashed holder, avoiding a permanent stall). Revisit only if the concurrent embed's CPU/memory spike (N × ~2.5 GB) proves painful.

### 7. Deployment (Helm — `constellation-runtime` chart)

Mirror the existing `keycloak-db.yaml` precedent:

- **New `embedding-db.yaml`:** single-replica `StatefulSet` (`replicas: 1`, `serviceName`, `volumeClaimTemplates` RWO PVC at `/var/lib/postgresql/data`, `PGDATA` subdir) running the stock `postgres` image; plus a headless Service `constellation-runtime-embedding-db:5432`. Gated on an `embeddingCache.enabled` values toggle (default off, matching `keycloak`/`hpa`/`pdb` conventions). Password from a Secret (managed-secrets/CSI in cloud overlays; kubectl-created Secret in kind), mirroring `dbPassword`. `pg_isready` readiness/liveness probes.
- **`mcp` container wiring (`deployment.yaml`):** add `EMBEDDING_CACHE_DATABASE_URL` (password from the Secret) and set the semantic `cache` config block (`type: postgres`, `url: ${env.EMBEDDING_CACHE_DATABASE_URL}`) in the mcp `/config`.
- **Why this dissolves C5:** the PVC lives on a dedicated single-replica StatefulSet (which *can* own `volumeClaimTemplates`), not on the 2-replica runtime `Deployment` (which can't). Both runtime replicas share the cache over the DB's headless Service — the same topology as the Keycloak Deployment → Keycloak-DB StatefulSet.
- **`startupProbe` unchanged:** the first cold replica still embeds (~140 s) before the shared cache is warm (runbook C3).
- **Sizing:** on-disk cache ≈ N × dim × 4 + overhead (~single-digit MB at this corpus; provision a small PVC, e.g. 1 Gi, with headroom for dead generations).

**Deploy-topology sub-decision (does not affect Rust code):** in-cluster `StatefulSet` Postgres (mirrors Keycloak precedent; chosen default) vs a managed Postgres (e.g. CloudSQL — no PVC to operate, connection string only). The MCP server only consumes a connection URL, so this is decidable at deploy time / per environment.

### 8. Testing

- **Unit (offline, fast suite):** the trait lets `VectorSearch::build`'s reuse/embed logic run against a `FakeEmbedder` + an in-memory `EmbeddingStore` test double (no SQLite/PG). The existing `second_build_reuses_cache_and_embeds_nothing` test carries over.
- **SQLite impl:** existing tests unchanged (roundtrip, meta mismatch wipes, persist across reopen, wrong-length rejection, stable content key).
- **Postgres impl (integration, `#[ignore]`d like the real-model test):** gated on `EMBEDDING_CACHE_DATABASE_URL`; CI runs it against a Postgres service container. Cases:
  - roundtrip put/get;
  - **generation isolation** — rows written under model-A are invisible to a model-B reader (and vice versa), no delete;
  - wrong-length rejection (rolls back, no partial write);
  - **idempotent concurrent `put`** — two writers, `ON CONFLICT DO NOTHING`, no error/corruption.
- **Helm:** `helm lint` + `helm template` + kind smoke for the new StatefulSet (per the chart's CONTRIBUTING loop).
- **Offline fail-open smoke (runbook):** `docker run --network=none` (DB unreachable) asserts the server comes up and logs embed/cache activity — lexical + in-memory semantic still work.

## Rollout / migration

- No data migration: the cache is a derived artifact. On first deploy with `type: postgres`, the cache is cold and populated by the first replica's embed.
- Switching a deployment from SQLite to Postgres (or vice versa) just repopulates from embedding; no export/import.
- Config change is breaking at the YAML level (`cache_path` → `cache` block), acceptable because Phase 2 is unshipped.

## Future work

- **Option B — session-scoped advisory-lock coordination (revisit).** The current
  approach (§6) only serializes the *startup/rollout* cold-embed via `maxSurge: 1`.
  A future enhancement is a Postgres **session advisory lock** keyed by the
  generation tuple, acquired in the build path before embedding the misses (then
  re-checking the cache after acquiring): exactly one replica embeds in *both*
  startup and runtime-rebuild scenarios, while every other replica waits briefly
  and then reads the now-warm cache. Use a **session** lock so a crashed holder
  auto-releases (no permanent stall). Prioritize this if the concurrent cold
  embed's resource spike (N × ~2.5 GB memory, N × ~140 s CPU) becomes a problem
  as replica count grows (e.g. under HPA).

## Open questions

- **TLS to Postgres:** plaintext in-cluster (Keycloak parity) vs required TLS. Default plaintext; add if security review requires.
- **In-cluster StatefulSet vs managed Postgres:** deploy-time choice; default in-cluster to match precedent.
- **Dead-generation pruning:** left to an out-of-band `DELETE WHERE model_id=...` (storage cost is negligible). A `last_used_at` column for automated lazy GC is a possible future add, not needed for correctness.

## Consequences

- Adds a `postgres` (sync) dependency to `apollo-schema-search` and a Postgres StatefulSet to the chart.
- The embedding cache is no longer node-local; a DB outage degrades gracefully to no-cache (cold embed), never below lexical + in-memory semantic.
- `pgvector`/Qdrant remain available as a future "Role B" step if corpus growth ever justifies in-DB ANN search; this design does not preclude it.
