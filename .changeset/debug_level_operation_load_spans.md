---
default: patch
---

# Stop exporting per-operation startup spans at the default log level

The spans that Apollo MCP Server creates while it turns each GraphQL operation into an MCP tool (`load_tool`, `operation_defs`, `tool_description`, `get_json_schema`, and others) are now `DEBUG`-level spans. At the default `INFO` log level, they're no longer exported to your tracing backend, so a large operation set no longer floods traces at startup and on every hot reload. To see them again, set `logging.level` to `debug`.
