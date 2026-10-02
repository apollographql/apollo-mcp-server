---
default: minor
---

# Account for scope hierarchies with `transport.auth.implied_scopes`

The MCP 2026-07-28 authorization spec requires servers to account for scope hierarchies, where a broader scope implies narrower ones, when deciding whether a token is sufficient. The new `transport.auth.implied_scopes` option maps a broader scope to the scopes it includes:

```yaml
transport:
  auth:
    implied_scopes:
      admin: [write]
      write: [read]
```

A token with a key also counts as having every listed scope, transitively, for both the global `scopes` check and `overrides.required_scopes`. The scopes advertised in Protected Resource Metadata and `WWW-Authenticate` challenges don't change.
