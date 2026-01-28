---
default: patch
---

# SSE Resumability Support

Upgraded rmcp to 0.14, which adds support for MCP Spec 2025-11-25 SSE resumability. When using HTTP transport with `stateful_mode: true` (the default), clients can now reconnect to SSE streams after disconnection using the `Last-Event-ID` header. The server automatically sends priming events with event IDs and retry intervals to enable this behavior.
