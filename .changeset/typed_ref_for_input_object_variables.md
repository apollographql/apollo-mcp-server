---
default: patch
---

# Emit `type` alongside `$ref` for input-object and enum variables

The JSON Schema emitted for a bare input-object variable used to be
`{ "$ref": "#/definitions/<Name>" }`, with no `type` sibling. In
strict draft-07 that is correct: the referenced schema is
authoritative, and any sibling to `$ref` is ignored.

Some MCP clients — including the transport used by VS Code's Copilot
MCP integration — do not dereference `$ref` before deciding how to
serialise a tool argument, and fall through to `JSON.stringify` on
whatever value the caller provides. The GraphQL server then rejects
the stringified payload with `Expected type <Name> to be an object`,
and the mutation never reaches the schema. Array-of-input-object
variables are unaffected because the property node already carries
`"type": "array"`.

This changeset adds `"type": "object"` next to the `$ref` for a
single input-object variable, and the corresponding `"type": "string"`
for a single enum variable. Correctly dereferencing clients ignore
the sibling and continue to use the full schema in `definitions`.
