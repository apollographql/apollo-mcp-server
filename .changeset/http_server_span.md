---
default: patch
---

# Emit a standards-compliant HTTP `SERVER` span for inbound requests

The span wrapping every request to the MCP endpoint was exported as `INTERNAL` and carried its HTTP data under names of this server's own invention, so backends that derive request and error metrics from span kind and status under-counted requests and missed server errors. That span is now a `SERVER` span named `{method} {route}`, with its status set to error on a 5xx response, following the OpenTelemetry [HTTP server span conventions](https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-server-span).

This renames the span from `mcp_server` and replaces its `method`, `uri` and `status_code` attributes with `http.request.method`, `url.path`, `url.scheme`, `http.route` and an integer `http.response.status_code`. The span also carries `server.address`, `server.port`, `user_agent.original`, `network.protocol.version`, `error.type` on a 5xx response, and `http.request.method_original` for a method outside the conventions' set — such a request reports `http.request.method` as `_OTHER` and is named `HTTP {route}`. The MCP session ID moves from `session_id` to `apollo.mcp.session_id`. Collector rules matching the old name or attributes need to change. Trace context propagation, baggage handling and the rest of the span tree are unchanged.
