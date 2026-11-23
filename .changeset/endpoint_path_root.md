---
default: minor
---

# Serve Streamable HTTP at the root path

`transport.path` now accepts `/`. The server then serves MCP at `/` only, so requests to other paths never reach MCP, and the health check and OAuth protected resource metadata stay at their own paths. A deployment whose public URL is the root, or whose proxy strips a prefix such as `/mcp`, no longer needs to rewrite every request onto a subpath.

```yaml
transport:
  type: streamable_http
  path: /
```
