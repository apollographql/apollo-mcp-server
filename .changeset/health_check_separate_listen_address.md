---
default: patch
---

# Support serving the health check on its own address/port

`health_check.listen` is a new, optional config field that serves the health check on its own
socket instead of merging it into the main `streamable_http` listener. This mirrors how the
Apollo Router exposes a separate health-check listen address, and is useful when the main MCP
endpoint sits behind authentication or host validation that shouldn't apply to health checks, or
when infrastructure like a Kubernetes readiness probe expects a dedicated port.

```yaml
health_check:
  enabled: true
  listen: 0.0.0.0:8088
```

When unset (the default), behaviour is unchanged: the health check is served on the same port as
the rest of the server.