---
default: minor
---

# Add `allow_anonymous_mcp_discovery` setting to allow unauthenticated access to MCP discovery methods (e.g. `tools/list`) when oauth is enabled

Example:

```yaml
transport:
  auth:
    allow_anonymous_mcp_discovery: true
```
