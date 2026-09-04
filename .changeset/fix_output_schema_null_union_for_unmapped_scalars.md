---
default: patch
---

# Fix output schema rejecting null for unmapped custom scalars and unknown types

With `overrides.enable_output_schema: true`, a nullable field's `outputSchema` wrapped its inner schema in `oneOf` with `{"type": "null"}`. For a custom scalar with no entry in `custom_scalars`, and for unknown types, the inner schema is `{}`, which already accepts `null`. A `null` response value therefore matched both `oneOf` branches, and MCP clients that validate `structuredContent` against `outputSchema` (such as the official MCP TypeScript SDK) rejected the tool result.

The null union now uses `anyOf`, which is the correct keyword for a union and has no such overlap requirement.
