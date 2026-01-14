---
default: minor
---

### Remove SSE transport support - @DaleSeo PR #555

The SSE transport has been removed following the upgrade to rmcp 0.12. SSE was previously deprecated in favor of Streamable HTTP transport.

**Migration**: Update your configuration to use `streamable_http` transport instead of `sse`:

- Before

```yaml
transport:
  type: sse
  address: 127.0.0.1
  port: 8000
```

- After

```yaml
transport:
  type: streamable_http
  address: 127.0.0.1
  port: 8000
```

If you were using SSE transport, switch to `streamable_http` which provides the same HTTP-based communication with improved session handling and is the recommended transport in the MCP specification.
