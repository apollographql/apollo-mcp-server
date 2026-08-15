---
default: patch
---

# Propagate W3C baggage through MCP Server telemetry

The server now uses OpenTelemetry's default composite propagator (W3C Trace Context plus W3C Baggage) so incoming `baggage` is extracted and outgoing GraphQL requests inject it. Default CORS `allow_headers` also includes `baggage` so browser clients can send the header on MCP requests.
