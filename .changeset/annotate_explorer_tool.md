---
default: patch
---

# Annotate the built-in `explorer` tool

The `explorer` tool now exposes MCP `ToolAnnotations` alongside the other built-ins: read-only, non-destructive, idempotent, and closed-world, since it only formats an Apollo Explorer URL and reaches nothing. Clients that gate auto-approval on `readOnlyHint` no longer prompt for it.

The docs now describe the `explorer` tool itself, and note that `overrides.annotations` applies to operation tools only — an entry named after a built-in tool is ignored.
