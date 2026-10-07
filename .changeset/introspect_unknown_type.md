---
default: patch
---

# Report unknown types during introspection

The `introspect` tool now returns an error with the requested type name when that type is not in the schema, instead of returning an empty successful result. Type names remain case-sensitive.
