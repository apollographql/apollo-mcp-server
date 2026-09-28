---
default: patch
---

# Reject `logging/setLevel` under protocol 2026-07-28

Prepare `logging/setLevel` to return JSON-RPC method-not-found (`-32601`) for
protocol 2026-07-28 and later, with HTTP 404 over Streamable HTTP. Older protocol
versions retain the empty-success no-op. The server continues to omit the logging
capability and does not emit MCP logging notifications.

This change does not enable protocol 2026-07-28: the production version cap remains
2025-11-25 until the broader protocol support is enabled separately.
