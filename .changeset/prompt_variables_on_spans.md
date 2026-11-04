---
default: minor
---

# Record prompt arguments on `prompts/get` spans

With `telemetry.exporters.tracing.gen_ai_content` enabled, the `prompts/get` span now records each argument the prompt declares as `gen_ai.prompt.variable.<name>`, following the OpenTelemetry MCP semantic conventions. The server skips arguments the prompt doesn't declare, so clients can't add attribute names. Prompt arguments can carry sensitive data, so they're off by default like the other GenAI content attributes.

If you already enable `gen_ai_content`, prompt arguments are exported after you upgrade. `omitted_attributes` can't remove them, so disable `gen_ai_content` if they must stay out of your traces.
