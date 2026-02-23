---
default: patch
---

# Fix JSON schema type mapping for GraphQL Int

Map GraphQL `Int` to JSON schema `{ "type": "integer" }` instead of `{ "type": "number" }`. This tells MCP clients to send integer values rather than floats (e.g. `1234` instead of `1234.0`), fixing input coercion errors on GraphQL servers that strictly validate `Int` inputs.
