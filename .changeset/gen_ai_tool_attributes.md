---
default: minor
---

# Record OpenTelemetry MCP and GenAI attributes on tool call spans

The `call_tool` span now records `mcp.method.name` (`tools/call`) from the OpenTelemetry semantic conventions for MCP. When the tool result has `isError: true`, the span also records `error.type` as `tool_error`, and when the call fails with a JSON-RPC server error, it records the error code. In both cases, the span status is set to error. The existing `apollo.mcp.*` attributes are unchanged.

Set `telemetry.exporters.tracing.gen_ai: true` to also export the OpenTelemetry GenAI attributes: `gen_ai.operation.name` (`execute_tool`), `gen_ai.tool.name`, `gen_ai.tool.call.arguments`, and `gen_ai.tool.call.result`. Observability platforms such as Langfuse use them to show a tool call's input and output. They're off by default because the arguments and result may contain sensitive data. `gen_ai.tool.call.result` is recorded only for a successful call.

Omitting `tool_name`, `tool_arguments`, or `tool_result` with `omitted_attributes` also removes the matching `gen_ai.*` attribute, even when `gen_ai` is enabled.
