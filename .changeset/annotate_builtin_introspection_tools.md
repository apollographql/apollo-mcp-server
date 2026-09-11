---
default: patch
---

# Annotate built-in introspection tools

The built-in `introspect`, `search`, `validate`, and `execute` tools now expose MCP `ToolAnnotations`. Schema-only tools are marked read-only, non-destructive, and idempotent. `execute` is read-only unless `mutation_mode` is `all`, and is always marked open-world because it calls the configured GraphQL endpoint.
