---
default: minor
---

# Add `discovery_headers` option to auth config

Add `discovery_headers` option to auth config for attaching custom headers to OIDC discovery and JWKS requests. This is useful when upstream OAuth servers or WAFs require headers like `User-Agent`.

```yaml
transport:
  type: streamable_http
  auth:
    servers:
      - https://auth.example.com
    resource: https://mcp.example.com
    scopes:
      - read
    discovery_headers:
      User-Agent: apollo-mcp-server
```
