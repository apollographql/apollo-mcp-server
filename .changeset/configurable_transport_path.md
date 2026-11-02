---
default: minor
---

# Configurable Streamable HTTP endpoint path

You can now serve the Streamable HTTP transport at a path other than `/mcp` with the new `transport.path` option, for example when a gateway or ingress expects a specific path. The default stays `/mcp`.

```yaml
transport:
  type: streamable_http
  path: /v1/agents
```

If you use auth, keep `transport.auth.resource` in line with the new path, for example `https://mcp.example.com/v1/agents`, so that clients accept the OAuth protected resource metadata. The server logs a warning at startup when the two don't match.
