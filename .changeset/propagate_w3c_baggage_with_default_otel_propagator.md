---
default: patch
---

# Propagate W3C baggage through MCP Server telemetry

Apollo MCP Server now registers a composite OpenTelemetry propagator containing W3C Trace Context and W3C Baggage so incoming `baggage` is extracted and outgoing GraphQL requests inject it. Default CORS `allow_headers` also includes `baggage` so browser clients can send the header on MCP requests.
