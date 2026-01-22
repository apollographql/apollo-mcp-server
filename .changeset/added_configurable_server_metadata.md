---
default: minor
---

# Added configurable server metadata

The MCP server now supports customizable metadata in the `initialize` response. Configure the server name, version, title, and website URL via the new `server_info` section in your configuration file. This is useful when wrapping or branding Apollo MCP Server for specific use cases.

```yaml
server_info:
  name: "Acme Corp GraphQL Server"
  version: "2.0.0"
  title: "Acme MCP Server"
  website_url: "https://acme.com/mcp-docs"
```

All fields are optional and fall back to sensible defaults.
