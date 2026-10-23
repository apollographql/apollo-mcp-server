---
default: patch
---

# Return an error when introspecting an unknown type

The `introspect` tool now returns a tool error that names the requested type when the type isn't in the schema. When a type differs only in letter case, the error suggests the correct name. Before, it returned an empty successful result, so clients couldn't tell a mistyped or missing type from a type with nothing to show.
