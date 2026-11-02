---
default: minor
---

# Record the recommended MCP attributes and links on every MCP handler span

Every MCP handler span now also records what the OpenTelemetry semantic conventions for MCP recommend for requests:

- `mcp.protocol.version` holds the request's MCP protocol version. On the `initialize` span, it's the version the server negotiated rather than the one the client asked for.
- `network.transport` is `tcp` for Streamable HTTP or `pipe` for stdio. Streamable HTTP requests also record `network.protocol.name` and `network.protocol.version`.
- `mcp.session.id` holds the session ID for a request on a session, the same value the HTTP request span records as `apollo.mcp.session_id`. Omitting `session_id` with `omitted_attributes` also omits `mcp.session.id`. Neither attribute is recorded for a request using protocol version `2026-07-28` or later, which has no session: the HTTP request span used to record whatever `Mcp-Session-Id` header such a request carried.
- When `_meta.traceparent` parents a handler span, the span links to the HTTP request span it arrived on, or to the `traceparent` header's context when that span is filtered out.
