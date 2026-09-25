---
default: patch
---

# Return GraphQL operation tools in deterministic order

Predefined GraphQL operation tools in `tools/list` are now sorted alphabetically by operation name using case-sensitive lexicographic ordering. With unique operation names, the order remains stable across repeated calls and schema or operation reloads with the same operations, supporting client-side tool-list caching and LLM prompt cache reuse. Operations with equal names retain their source order. Built-in and app tools retain their existing order after the predefined operations.
