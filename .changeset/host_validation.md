---
default: minor
---

Add Host header validation to prevent DNS rebinding attacks. Requests with invalid Host headers are now rejected with 403 Forbidden. Enabled by default for StreamableHttp transport.

```yaml
transport:
  type: streamable_http
  host_validation:
    enabled: true # default
    allowed_hosts:
      - mcp.dev.example.com
      - mcp.staging.example.com
      - mcp.example.com
```
