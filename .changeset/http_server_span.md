---
default: patch
---

# Emit a standards-compliant HTTP `SERVER` span for inbound requests

The span wrapping every request to the MCP endpoint was exported as `INTERNAL` and carried its HTTP data under names of this server's own invention, so backends that derive request and error metrics from span kind and status under-counted requests and missed server errors. That span is now a `SERVER` span named `{method} {route}`, with its status set to error on a 5xx response, following the OpenTelemetry [HTTP server span conventions](https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-server-span).

Collector rules, dashboards and monitors that match the old span need to change:

| Before | After |
| --- | --- |
| span name `mcp_server` | `{method} {route}`, for example `POST /mcp` |
| span kind `INTERNAL` | `SERVER` |
| `method` | `http.request.method` |
| `uri` | `url.path` |
| `status_code`, a string such as `"200 OK"` | `http.response.status_code`, the integer `200` |
| `session_id` | `apollo.mcp.session_id` |

The span also carries `server.address`, `server.port`, `user_agent.original`, `network.protocol.version`, `error.type` on a 5xx response, and `http.request.method_original` for a method outside the conventions' set — such a request reports `http.request.method` as `_OTHER` and is named `HTTP {route}`.

Requests that stream a response — every tool call — now report the full request duration rather than the time to the response head, because the span stays open until the body finishes. This is the duration the span should always have carried, but it is several times larger than the old one, so alert thresholds and latency panels built on the previous number need re-baselining. A `GET` on the MCP endpoint is unaffected: it is the session's standing server-to-client stream, and its span still ends at the response head.

Trace context propagation, baggage handling and the rest of the span tree are unchanged.
