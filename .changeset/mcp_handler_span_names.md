---
default: minor
---

# Name MCP handler spans per the MCP semantic conventions

MCP handler spans now follow the OpenTelemetry MCP semantic conventions: each one is named after its MCP method and has span kind `SERVER` instead of `INTERNAL`. A `tools/call` span adds the tool name, and a `prompts/get` span adds the prompt name. When a request fails with a caller error, such as an unknown tool, or `omitted_attributes` lists `tool_name`, the name is the method alone. As the [telemetry docs](https://www.apollographql.com/docs/apollo-mcp-server/telemetry#semantic-conventions) note, handler span names are experimental: they track the MCP semantic conventions, which are still in development, so they may change in a future release.

**Migration**: Update any dashboard, alert, or query that filters on the old span names. To match a handler span by method regardless of its target, filter on the `mcp.method.name` attribute instead. Text and JSON logs keep the old span names.

| Before | After |
| --- | --- |
| `call_tool` | `tools/call {tool name}` |
| `get_prompt` | `prompts/get {prompt name}` |
| `list_tools` | `tools/list` |
| `list_resources` | `resources/list` |
| `list_resource_templates` | `resources/templates/list` |
| `read_resource` | `resources/read` |
| `list_prompts` | `prompts/list` |
| `set_level` | `logging/setLevel` |
| `listen` | `subscriptions/listen` |
| `get_task`, `cancel_task`, `update_task` | `tasks/get`, `tasks/cancel`, `tasks/update` |
| `initialize` | `initialize` (unchanged) |

Each MCP request over Streamable HTTP now has two `SERVER` spans: the HTTP request span, and the MCP handler span nested under it or parented by `_meta` trace context. Queries that assumed one `SERVER` span per request should also filter on `http.request.method` or `mcp.method.name`.
