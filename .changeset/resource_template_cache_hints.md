---
default: patch
---

# Include cache hints on resource template lists

Empty `resources/templates/list` responses now include the configured `ttlMs`
and private `cacheScope` for MCP 2026-07-28 clients. Earlier protocol versions
retain their existing response shape.
