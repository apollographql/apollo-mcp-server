---
default: patch
---

# Emit the mapped `type` alongside `$ref` for custom scalar variables

A custom scalar variable used to produce a bare `{ "$ref": "#/definitions/<Name>" }` in the tool's input schema, even when its `custom_scalars` mapping declares a `type`. Clients that don't dereference `$ref`, including the Claude connector directory validator, couldn't classify the parameter.

When the mapping declares a top-level `type`, the server now emits it next to the `$ref`, as it already does for enum and input-object variables. Scalars without a mapping, or whose mapping has no `type`, still produce a bare `$ref`.
