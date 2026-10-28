---
default: patch
---

# Trace MCP Tasks requests

The `tasks/get`, `tasks/cancel`, and `tasks/update` handlers now create spans (`get_task`, `cancel_task`, and `update_task`) like every other MCP handler, so task polling and cancellation show up in traces with the request's trace context. Each span records `mcp.method.name`. The task ID is never recorded, because it can act as a bearer handle.
