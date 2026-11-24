---
default: minor
---

# Serve Streamable HTTP at the exact `transport.path` only

The server now serves MCP only at the exact `transport.path`, as it already did for the root path `/`. Before, a path such as `/mcp` also served MCP at every subpath such as `/mcp/anything`. Those requests now return HTTP 404.

Trailing slashes are ignored, as in Apollo Router. A request to `/mcp/` still reaches `/mcp`, and `transport.path` now accepts a trailing slash, so `/mcp/` configures the same endpoint as `/mcp`. `health_check.path` now follows the same rules as `transport.path`.
