---
default: minor
---

# Emit the `mcp.server.operation.duration` metric

The server now emits `mcp.server.operation.duration`, the request duration histogram from the OpenTelemetry semantic conventions for MCP. It records every MCP request in seconds, except `ping`, `server/discover`, and `completion/complete`, with `mcp.method.name`, `mcp.protocol.version`, the network attributes for the transport, the tool or prompt name, and `error.type` and `rpc.response.status_code` when the request fails. The existing `apollo.mcp.*` metrics are unchanged.
