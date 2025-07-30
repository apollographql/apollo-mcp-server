### feat: add mcp auth - @nicholascioli PR #210

The MCP server can now be configured to act as an OAuth 2.1 resource server, following
guidelines from the official MCP specification on Authorization / Authentication (see
[the spec](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization)).

To configure this new feature, a new `auth` section has been added to the SSE and
Streamable HTTP transports. Below is an example configuration using Streamable HTTP:

```yaml
transport:
  type: streamable_http
  auth:
    # List of upstream delegated OAuth servers
    # Note: These need to support the OIDC metadata discovery endpoint
    servers:
    - https://auth.example.com

    # List of accepted audiences from upstream signed JWTs
    # See: https://www.ory.sh/docs/hydra/guides/audiences
    audiences:
    - mcp.example.audience

    # The externally available URL pointing to this MCP server. Can be `localhost`
    # when testing locally.
    # Note: Subpaths must be preserved here as well. So append `/mcp` if using
    # Streamable HTTP or `/sse` is using SSE.
    resource: https://hosted.mcp.server/mcp

    # Optional link to more documentation relating to this MCP server.
    resource_documentation: https://info.mcp.server

    # List of queryable OAuth scopes from the upstream OAuth servers
    scopes:
    - read
    - mcp
    - profile
```
