---
default: patch
---

# Improve search recall by indexing schema fields instead of types

The schema index now writes one Tantivy document per field (or enum value) rather than one document per type, with the parent type, field name, arg names, return type, and merged description as searchable text. Field-name matches receive a per-token boost.

This fixes cases where searching for a specific operation (for example, `userByEmail` against a multi-subgraph supergraph) returned unrelated types whose field-list text happened to contain more of the constituent tokens. The `search` API and `PathNode` output format are unchanged; paths still start at a root operation type and walk down to the matched field.
