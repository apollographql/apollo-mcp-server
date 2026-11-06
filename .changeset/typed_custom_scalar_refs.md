---
default: patch
---

# Add the mapped type to custom scalar references

A custom scalar mapped through `custom_scalar_map` is referenced with a bare `$ref` in input schemas. Clients that do not dereference `$ref` cannot tell how to serialize its value. The reference now carries the mapped `type`, such as `{"type": "string", "$ref": "#/definitions/Date"}`, like enum and input object references already do. Scalars without a mapping keep a bare `$ref`.
