---
default: patch
---

# Update rmcp to 3.8.0

With `stateful_mode` enabled, a Streamable HTTP POST that has no `Mcp-Session-Id` header and is not `initialize` is now rejected with HTTP 400 instead of 422, matching the spec and the existing response for GET and DELETE.

The update also picks up rmcp's transport and model fixes from 3.6.0 through 3.8.0, including a per-request tool schema lookup in the Streamable HTTP server and stricter rejection of malformed JSON-RPC messages.
