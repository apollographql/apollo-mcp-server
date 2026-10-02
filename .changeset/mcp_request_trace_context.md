---
default: patch
---

# Propagate MCP request trace context

Read W3C `traceparent`, `tracestate`, and `baggage` from incoming request `_meta`
over stdio and Streamable HTTP. Valid metadata trace context parents the MCP
handler span directly; otherwise HTTP span parentage is retained. Metadata
baggage replaces HTTP baggage when present, and safe context propagates to
downstream GraphQL requests. Invalid telemetry values do not fail requests.
Context propagation and Rhai trace-ID correlation also work when logging
filters disable request spans.
