---
default: minor
---

# Link MCP handler spans to their HTTP request span

When `_meta.traceparent` parents an MCP handler span, the span now also carries a span link to the HTTP request span it arrived on, as the OpenTelemetry semantic conventions for MCP recommend. Before, a handler parented through `_meta` had no connection to its transport. When the HTTP request span is filtered out, the link points to the context from the request's `traceparent` header instead.
