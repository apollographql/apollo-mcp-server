---
default: patch
---

# Send stdio transport logs to stderr

When `transport.type` is `stdio` and `logging.path` is not set, log output is written to stderr instead of stdout. The stdio transport reserves stdout for MCP JSON-RPC messages, and log lines on stdout could corrupt the message stream for MCP clients. The `streamable_http` transport continues to log to stdout.
