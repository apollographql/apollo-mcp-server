---
default: minor
---

# Cache semantic-search embeddings in Postgres across restarts

When `introspection.search.semantic.cache_url` is set to a Postgres connection
string, operation embeddings are persisted to that database and reused on
subsequent starts instead of being recomputed. Because the store is shared, every
MCP replica reuses one durable cache, so the corpus is embedded once per
(embedding-generation, operation) rather than once per pod per restart. Vectors
are content-addressed by the SHA-256 of each operation's document text, so only
added or changed operations are re-embedded; rows are additionally keyed by the
exact `(model_id, dim, dtype, doc_builder_ver)` generation tuple, so a model,
dimensionality, or document-format change transparently starts a new generation
without deleting or colliding with existing rows. Concurrent writers are
idempotent (`ON CONFLICT DO NOTHING`). The cache is fail-open: any connection or
I/O error falls back to embedding from scratch. Unset = previous behavior (embed
on every start).
