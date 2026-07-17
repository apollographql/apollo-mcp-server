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
