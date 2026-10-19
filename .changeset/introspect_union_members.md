---
default: patch
---

# List union members in depth-limited introspection

The `introspect` tool now lists every member of a union even when the depth limit stops the member types from being expanded, so the default `depth: 1` returns `union SearchResult = User | Post | ...` instead of a union with no members. The depth limit still controls whether member type definitions are included. Union members omitted from operation tool descriptions because no fragment selects them are unchanged.
