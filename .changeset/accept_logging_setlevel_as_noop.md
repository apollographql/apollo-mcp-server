---
default: patch
---

# Accept `logging/setLevel` requests as a no-op

Apollo MCP Server now accepts `logging/setLevel` requests with an empty success response. Previously, clients that eagerly call `setLevel` immediately after `initialize` (notably the MCPJam inspector) received a `-32601 Method not found` response, which surfaced as a red error row in the inspector's logging panel and led users to believe their server was broken. The server does not advertise the `logging` capability and does not stream `notifications/message` to clients. The requested level has no effect on the existing stderr, file, and OpenTelemetry logging pipeline, which is the path recommended by the upcoming MCP [2026-07-28](https://modelcontextprotocol.io/specification/draft/server/utilities/logging) revision that deprecates this feature.
