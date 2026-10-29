---
default: minor
---

# Record MCP request attributes and server errors on every MCP handler span

Every MCP handler span now records the attributes that the OpenTelemetry semantic conventions for MCP require for requests:

- `jsonrpc.request.id` holds the JSON-RPC request ID on every handler span. Omitting `request_id` with `omitted_attributes` also omits `jsonrpc.request.id`.
- `error.type` and an error span status mark a handler that fails with a server error, not just `call_tool`. Errors caused by the caller, such as an unknown method or invalid parameters, leave the status unset.
- `mcp.resource.uri` holds the resource URI on the `read_resource` span, and `gen_ai.prompt.name` holds the prompt name on the `get_prompt` span.
