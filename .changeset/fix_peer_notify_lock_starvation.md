---
default: patch
---

# Fix server becoming unresponsive due to zombie peer lock starvation

The MCP server could become completely unresponsive to POST /mcp requests after hours of uptime while /health remained responsive. This occurred when a peer's transport entered a half-closed state (e.g., from an HTTP/2 RST_STREAM) and a subsequent schema or operations update tried to notify the zombie peer while holding the operations write lock — blocking all tool listing, tool calling, and session initialization indefinitely.

The operations write lock is now released before notifying peers, and each peer notification has a 5-second timeout. Unresponsive peers are dropped instead of blocking the server.
