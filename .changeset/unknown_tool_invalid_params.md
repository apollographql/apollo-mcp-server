---
default: patch
---

# Report unknown tools as invalid params

Calling a tool that doesn't exist now returns JSON-RPC error `-32602` (Invalid params), as the MCP tools specification requires. Previously the server returned `-32601` (Method not found). On the `2026-07-28` Streamable HTTP transport, `-32601` is sent with HTTP 404, which tells a client that the server doesn't implement `tools/call` at all.
