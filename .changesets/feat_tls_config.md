### Add TLS configuration options for auth - @DaleSeo PR #536

Adds TLS configuration options for connecting to OAuth servers during token validation.

When the MCP server validates OAuth tokens, it connects to upstream OAuth servers to fetch JWKS keys. Previously, this required those servers to have certificates trusted by the system's default CA bundle. This change allows users to trust custom CA certificates or disable validation for development environments.

```yaml
transport:
  streamable_http:
    auth:
      servers:
        - https://auth.example.com
      audiences:
        - my-audience
      resource: https://mcp.example.com/mcp
      tls:
        ca_cert: /path/to/ca-certificate.pem
        danger_accept_invalid_certs: false  # Set this to true for development or testing purposes only
```