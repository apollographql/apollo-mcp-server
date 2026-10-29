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

If you use auth, set `transport.auth.resource` to a URL that ends with the same path, so that the OAuth protected resource metadata is served at the matching well-known URL.
