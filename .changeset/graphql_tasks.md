---
default: minor
---

# Return MCP Tasks for slow GraphQL calls

GraphQL tool calls that take longer than a configurable threshold can now return an [MCP Tasks](https://tasks.extensions.modelcontextprotocol.io/seps/2663-tasks-extension) handle. Clients then poll `tasks/get` for the result or stop the call with `tasks/cancel`. The feature is off by default. It applies only to clients that declare Tasks support:

```yaml
tasks:
  enabled: true
  detach_after_ms: 1000
```

Tasks are held in server memory, and fast calls return their results as before.
