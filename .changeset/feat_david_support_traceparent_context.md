---
default: minor
---

### Server adds support for incoming distributed trace context propagation - @david-castaneda PR #484

The MCP server now extracts W3C traceparent headers from incoming requests and uses this context for its own emitted traces, enabling handler spans to nest under parent traces for complete end-to-end observability.
