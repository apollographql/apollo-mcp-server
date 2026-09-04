---
default: patch
---

# Fix output schema rejecting valid responses for nullable and union fields

With `overrides.enable_output_schema: true`, a nullable field's `outputSchema` wrapped its inner schema in `oneOf` with `{"type": "null"}`. For a custom scalar with no entry in `custom_scalars`, and for unknown types, the inner schema is `{}`, which already accepts `null`. A `null` response value therefore matched both `oneOf` branches, and MCP clients that validate `structuredContent` against `outputSchema` (such as the official MCP TypeScript SDK) rejected the tool result.

GraphQL union fields had the same problem. Each inline fragment became a `oneOf` branch, but the member schemas carry no discriminator and allow extra properties, so a response object that satisfies more than one fragment (for example when fragments select the same field names) also failed validation.

Both places now use `anyOf`, which is the correct keyword for a union and has no exclusivity requirement.
