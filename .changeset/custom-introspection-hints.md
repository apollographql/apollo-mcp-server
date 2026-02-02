---
default: minor
---

# Added configurable hints for introspection tools

Apollo MCP Server now supports configurable hint text for the built-in introspection tools (`execute`, `introspect`, `search`, and `validate`). These hints are appended to the tool descriptions so you can guide query generation without changing schema descriptions.

```yaml
introspection:
  execute:
    enabled: true
    description: "Use carts(where: { status: ACTIVE }) for active carts."
```
