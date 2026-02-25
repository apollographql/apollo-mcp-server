---
default: patch
---

# Gate outputSchema and structuredContent on negotiated MCP protocol version

When `enable_output_schema` is configured, the server now advertises MCP protocol version `2025-06-18` and only includes `outputSchema` in `tools/list` and `structuredContent` in `tools/call` responses when the client negotiates a protocol version that supports them (`>= 2025-06-18`). Previously, these fields were sent regardless of the negotiated version, which could cause errors in clients that don't recognize them.
