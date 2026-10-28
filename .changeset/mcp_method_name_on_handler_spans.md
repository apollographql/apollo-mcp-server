---
default: minor
---

# Record `mcp.method.name` on every MCP handler span

Every MCP handler span, not just `call_tool`, now records the request's method as `mcp.method.name`, as the OpenTelemetry semantic conventions for MCP require. For example, the `list_tools` span has `mcp.method.name` set to `tools/list`, and the `read_resource` span has `resources/read`. Span names are unchanged.
