# Design — Externalize the embedding cache to Postgres

**Date:** 2026-07-21 · **Branch:** `air-311-hybrid-search`
**Relates to:** [ONNX/semantic CI-CD runbook](../notes/2026-07-17-onnx-semantic-search-cicd-deployment.md)

## Summary

The semantic-search embedding cache is **content-addressed** (a vector per operation, keyed by the SHA-256 of its document text). Persisting it across pod restarts is the deploy problem: a node-local file cache cannot be shared across a **2-replica `Deployment`**, which cannot use per-replica `volumeClaimTemplates` (a StatefulSet feature) and cannot share a single `ReadWriteOnce` PVC across pods.

This design **stores the cache in Postgres**, deployed as a dedicated single-replica `StatefulSet` that mirrors the existing `keycloak-db` precedent in the `constellation-runtime` Helm chart. The stateful storage lives on the StatefulSet's own PVC; all MCP replicas share the cache over the DB's headless Service. This:

- removes the per-pod PVC requirement from the runtime `Deployment` entirely;
- makes the cache durable across pod lifecycle and shareable across replicas — the first replica to boot on a new schema embeds and writes, the rest read;
- lets the datastore scale/operate independently of the MCP server;
- reuses infrastructure and operational patterns the team already runs (Keycloak's Postgres StatefulSet).

**Scope: cache only ("Role A").** The datastore stores precomputed embeddings; the MCP server continues to load vectors into memory and perform exact in-memory brute-force cosine search. Offloading vector *search* into a vector DB (Qdrant/pgvector) is explicitly **out of scope** — see Non-Goals.

## Goals

- Embeddings persist across restarts and are shared by all MCP replicas, so the ~140 s ONNX embed is paid once per (embedding-semantics, operation), not once per pod per restart.
- No stateful storage on the multi-replica runtime `Deployment`.
- The embedding datastore is deployed and scaled independently of the MCP server.
- Preserve the cache guarantees: content-addressed incremental reuse, fail-open, and correct invalidation when embedding semantics change.
- Keep the reuse/embed logic testable offline (no live DB) via a trait seam.

## Non-Goals

- **Offloading vector search into the DB** (Qdrant / pgvector ANN). At the current corpus size (hundreds to low-thousands of operations; ~1.5 KB per 384-dim vector; single-digit MB total), in-memory brute-force cosine is already exact and sub-millisecond. ANN indexes solve approximate search at scale — a problem this corpus does not have. Revisit only if the corpus grows to ~100k+ operations or per-replica vector memory becomes a concern.
- **Having the datastore compute embeddings.** Self-hosted Qdrant/Postgres do not embed; the ONNX embed stays in the MCP server regardless of datastore. This design does not change where embedding happens.
- **Removing the cold-start embed.** A shared store removes the *repetition* of embedding, not the first embed. The `startupProbe` tolerating the cold embed (runbook C3) still applies.
- Advisory-lock-based single-embedder coordination (deferred; see Coordination).

## Carried-over cache concepts

These predate this change and are preserved by the Postgres backend:

- **Content key:** `op_key = hex(SHA-256(OperationDocument.text))` — changed op → miss → re-embed; unchanged op → reuse. Stable across schema sources.
- **Global invalidation** when embedding semantics change, keyed on `(model_id, dim, dtype, doc_builder_ver)` (`DOC_BUILDER_VERSION` bumps when the doc-text format changes).
- **Vector serialization:** raw little-endian f32 (`dim×4` bytes; `dtype = "f32le"`).
- **Two-pass build** (`VectorSearch::build`): pass 1 reuses cache hits, pass 2 embeds only misses and persists them.

## Design

### 1. `EmbeddingStore` trait (backend abstraction)

The cache lives behind a trait so the backend is swappable and testable:

```rust
pub trait EmbeddingStore: Send {
    /// Fetch a cached vector by content key. `Ok(None)` = miss.
    fn get(&mut self, key: &str) -> Result<Option<Vec<f32>>, CacheError>;
    /// Insert/replace `(key, vector)` pairs idempotently.
    fn put_batch(&mut self, entries: &[(String, Vec<f32>)]) -> Result<(), CacheError>;
}
```

- `&mut self` on both methods: the synchronous `postgres::Client` requires `&mut` to query.
- Production impl: `PostgresCache` (see §2–§3). Tests use an in-memory `EmbeddingStore` double, so the reuse/embed logic is covered offline without a live DB.
- Shared free functions (no duplication): `doc_key`, `vec_to_blob`, `blob_to_vec` (with its length validation), `DOC_BUILDER_VERSION`.
- `VectorSearch::build` takes `Option<&mut dyn EmbeddingStore>`; the two-pass reuse/embed logic only calls `get`/`put_batch`.

### 2. Postgres data model — generation-keyed (exact-tuple composite key)

The Postgres cache is shared by all replicas, so a destructive global "meta guard" (delete-all-on-mismatch) is unsafe: during a rollout that changes the embedding model or doc format, an old-model pod and a new-model pod would each delete the other's vectors and re-embed, ping-ponging until the rollout completes.

Instead, **partition rows by embedding-semantics generation using the exact tuple as part of the primary key** — no destructive delete, no cross-writer contention:

```sql
CREATE TABLE IF NOT EXISTS embeddings (
  model_id        text    NOT NULL,
  dim             integer NOT NULL,
  dtype           text    NOT NULL,
  doc_builder_ver bigint  NOT NULL,
  op_key          text    NOT NULL,   -- hex(SHA-256(doc text))
  vector          bytea   NOT NULL,   -- raw little-endian f32, dim*4 bytes
  PRIMARY KEY (model_id, dim, dtype, doc_builder_ver, op_key)
);
```

- **No hashing on the generation axis.** The generation is the exact `(model_id, dim, dtype, doc_builder_ver)` tuple, so generation collisions are impossible by construction. (A *truncated* generation hash could collide two same-dimension models and silently serve the wrong vector; an exact composite key eliminates that class entirely.)
- `op_key` is a **full SHA-256**; collision probability is cryptographically negligible.
- Reads/writes always scope to the *current* tuple; rows from other generations are inert (harmless), and prunable out-of-band (`DELETE WHERE model_id = '<old>'`) without racing live writers.
- Vectors are a raw `bytea` blob (`vec_to_blob`/`blob_to_vec`) — no `pgvector` extension, stock `postgres` image suffices.

**Defense-in-depth:** `blob_to_vec` rejects any blob whose length ≠ `dim×4`, and `build` treats a `get` error as a miss and re-embeds. So any dimension mismatch fails safe; the exact-tuple key closes the only remaining (same-dim, different-model) window.

### 3. `PostgresCache` behavior

- **Client:** the synchronous `postgres` crate, keeping `VectorSearch::build` synchronous. In-cluster connections are plaintext (`NoTls`) by default, mirroring `keycloak-db`. TLS is a configurable add-on (deferred unless required).
- **`open`/connect:** connect using the configured URL; `CREATE TABLE IF NOT EXISTS` (idempotent, safe for concurrent replicas). Store the current generation tuple on the struct. Failure to connect → caller falls back to no cache (fail-open, §5).
- **`get(key)`:** `SELECT vector FROM embeddings WHERE model_id=$1 AND dim=$2 AND dtype=$3 AND doc_builder_ver=$4 AND op_key=$5`; deserialize + length-validate via `blob_to_vec`.
- **`put_batch(entries)`:** one transaction; per-entry length validated against `dim` before insert (mismatch → `CacheError::BadVectorLen`, transaction dropped uncommitted → rollback, no partial write); `INSERT ... ON CONFLICT (model_id, dim, dtype, doc_builder_ver, op_key) DO NOTHING` so concurrent writers are idempotent. Commit only after all inserts.

### 4. Configuration surface

A single optional field under `introspection.search.semantic`:

```yaml
introspection:
  search:
    semantic:
      # omit => no cache (embed on every start), the default
      cache_url: ${env.EMBEDDING_CACHE_DATABASE_URL}
```

- `cache_url` is a Postgres connection string (libpq or URL form). The password is sourced from an env var backed by a k8s Secret and never inlined (mirrors Keycloak's `dbPassword`); YAML `${env.VAR}` expansion already supports this.
- `Search` retains the resolved `cache_url` so `rebuild` reopens the store fail-open across schema reloads.
- `build_backend` opens the store from `cache_url` (`open_store` → `Option<PostgresCache>`); unset or connect-failure → no cache.

### 5. Error handling — fail-open preserved

Three-layer fail-open:

1. **Open/connect failure** → run with no cache (embed in-memory from scratch, warn). Server always comes up.
2. **`get` error** → treated as a miss (re-embed that op).
3. **`put_batch` error** → log + continue (results still served; just not persisted this round).

`CacheError` has `Backend(String)` (Postgres errors) and `BadVectorLen`. A DB outage never degrades below lexical + in-memory-semantic behavior.

### 6. Off-runtime build (correctness)

The index build — ONNX model load, embedding, and the **synchronous** Postgres client — is blocking work, and the sync `postgres` client internally calls `block_on`, which panics if run on an async runtime thread. So the whole build runs off the async runtime via `tokio::task::spawn_blocking`, in **both** initial construction (`Search::new`) and schema-reload (`rebuild`). This also keeps the ~140 s cold embed from stalling the executor.

### 7. Multi-writer coordination

Writes are idempotent (`ON CONFLICT DO NOTHING`), so concurrent writers are always safe — no corruption; at worst two replicas embed the same miss once.

- **Rolling update (the common case):** pods restart one at a time. The first new pod embeds + writes; later pods read the now-warm shared cache.
- **Full-fleet cold start** (brand-new schema generation, or all pods down): several replicas may embed concurrently — correct, bounded, self-healing (next restart is warm).
- **Decision (for now): rely on rolling-update serialization only.** Set the runtime Deployment's rollout to one-at-a-time (`maxSurge: 1`) so a single cold-embedder warms the shared cache before the next pod starts. Deploy-side (Helm) only — no application code.
- **Not handled by the above:** a *runtime* schema change (uplink pushes a new supergraph to already-running replicas) makes all live replicas `rebuild` concurrently; k8s ordering cannot serialize that. Accepted for now (idempotent writes keep it correct, just redundant).
- **Advisory locks deferred** — see Future work.

### 8. Deployment (Helm — `constellation-runtime` chart)

Mirror the existing `keycloak-db.yaml` precedent:

- **New `embedding-db.yaml`:** single-replica `StatefulSet` (`replicas: 1`, `serviceName`, `volumeClaimTemplates` RWO PVC at `/var/lib/postgresql/data`, `PGDATA` subdir) running the stock `postgres` image; plus a headless Service `constellation-runtime-embedding-db:5432`. Gated on an `embeddingCache.enabled` values toggle (default off). Password from a Secret (mirroring `dbPassword`). `pg_isready` readiness/liveness probes.
- **`mcp` container wiring (`deployment.yaml`):** add `EMBEDDING_CACHE_DATABASE_URL` (password from the Secret) and set `introspection.search.semantic.cache_url: ${env.EMBEDDING_CACHE_DATABASE_URL}` in the mcp `/config`.
- **Why this dissolves the PVC problem:** the PVC lives on a dedicated single-replica StatefulSet (which *can* own `volumeClaimTemplates`), not on the 2-replica runtime `Deployment` (which can't). Both runtime replicas share the cache over the DB's headless Service — the same topology as the Keycloak Deployment → Keycloak-DB StatefulSet.
- **`startupProbe` unchanged:** the first cold replica still embeds (~140 s) before the shared cache is warm.
- **Sizing:** on-disk cache ≈ N × dim × 4 + overhead (single-digit MB at this corpus; provision a small PVC, e.g. 1 Gi).

**Deploy-topology sub-decision (does not affect Rust code):** in-cluster `StatefulSet` Postgres (mirrors Keycloak precedent; chosen default) vs a managed Postgres (e.g. CloudSQL — no PVC to operate, connection string only). The MCP server only consumes a connection URL, so this is decidable at deploy time / per environment.

### 9. Testing

- **Unit (offline, fast suite):** the trait lets `VectorSearch::build`'s reuse/embed logic run against a `FakeEmbedder` + an in-memory `EmbeddingStore` double (no live DB) — `build_reuses_via_trait_object_store` proves a warm store embeds nothing.
- **`PostgresCache` (integration, `#[ignore]`d like the real-model test):** gated on `EMBEDDING_CACHE_DATABASE_URL`; CI runs it against a Postgres service container. Cases: roundtrip put/get; **generation isolation** (model-A rows invisible to a model-B reader, no delete); wrong-length rejection (rolls back, no partial write); **idempotent concurrent `put`**.
- **Tool-level fail-open:** an unreachable `cache_url` still builds the tool (degrades to no-cache), verified without a live DB.
- **Helm:** `helm lint` + `helm template` + kind smoke for the new StatefulSet.
- **Offline fail-open smoke:** `docker run --network=none` (DB unreachable) asserts the server comes up — lexical + in-memory semantic still work.

## Rollout / migration

- No data migration: the cache is a derived artifact. On first deploy with `cache_url` set, the cache is cold and populated by the first replica's embed.
- `cache_url` unset preserves prior behavior (embed on every start).

## Future work

- **Option B — session-scoped advisory-lock coordination (revisit).** The current approach (§7) only serializes the *startup/rollout* cold-embed via `maxSurge: 1`. A future enhancement is a Postgres **session advisory lock** keyed by the generation tuple, acquired in the build path before embedding the misses (then re-checking the cache after acquiring): exactly one replica embeds in *both* startup and runtime-rebuild scenarios, while every other replica waits briefly and then reads the now-warm cache. Use a **session** lock so a crashed holder auto-releases (no permanent stall). Prioritize if the concurrent cold-embed resource spike (N × ~2.5 GB memory, N × ~140 s CPU) becomes a problem as replicas scale (e.g. under HPA).

## Open questions

- **TLS to Postgres:** plaintext in-cluster (Keycloak parity) vs required TLS. Default plaintext; add if security review requires.
- **In-cluster StatefulSet vs managed Postgres:** deploy-time choice; default in-cluster to match precedent.
- **Dead-generation pruning:** left to an out-of-band `DELETE WHERE model_id=...` (storage cost is negligible). A `last_used_at` column for automated lazy GC is a possible future add, not needed for correctness.

## Consequences

- Adds a `postgres` (sync) dependency to `apollo-schema-search` and a Postgres StatefulSet to the chart.
- The embedding cache is no longer node-local; a DB outage degrades gracefully to no-cache (cold embed), never below lexical + in-memory semantic.
- `pgvector`/Qdrant remain available as a future "Role B" step if corpus growth ever justifies in-DB ANN search; this design does not preclude it.
