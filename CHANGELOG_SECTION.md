# [1.1.0] - 2025-10-16

## ❗ BREAKING ❗

### Change default port from 5000 to 8000 - @DaleSeo PR #417

The default server port has been changed from `5000` to `8000` to avoid conflicts with common development tools and services that typically use port 5000 (such as macOS AirPlay, Flask development servers, and other local services).

**Migration**: If you were relying on the default port 5000, you can continue using it by explicitly setting the port in your configuration file or command line arguments.

- Before 

```yaml
transport:
  type: streamable_http
```

- After

```yaml
transport:
  type: streamable_http
  port: 5000
```

## 🚀 Features

### feat: Add configuration option for metric temporality - @swcollard PR #413

Creates a new configuration option for telemetry to set the Metric temporality to either Cumulative (default) or Delta.

* Cumulative - The metric value will be the overall value since the start of the measurement.
* Delta - The metric will be the difference in the measurement since the last time it was reported.

Some observability  vendors require that one is used over the other so we want to support the configuration in the MCP Server.

### Add support for forwarding headers from MCP clients to GraphQL APIs - @DaleSeo PR #428

Adds opt-in support for dynamic header forwarding, which enables metadata for A/B testing, feature flagging, geo information from CDNs, or internal instrumentation to be sent from MCP clients to downstream GraphQL APIs. It automatically blocks hop-by-hop headers according to the guidelines in [RFC 7230, section 6.1](https://datatracker.ietf.org/doc/html/rfc7230#section-6.1), and it only works with the Streamable HTTP transport.

You can configure using the `forward_headers` setting:

```yaml
forward_headers:
  - x-tenant-id
  - x-experiment-id
  - x-geo-country
```

Please note that this feature is not intended for passing through credentials as documented in the best practices page.

### feat: Add mcp-session-id header to HTTP request trace attributes - @swcollard PR #421

Includes the value of the [Mcp-Session-Id](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#session-management) HTTP header as an attribute of the trace for HTTP requests to the MCP Server

## 🐛 Fixes

### Fix compatibility issue with VSCode/Copilot - @DaleSeo PR #447

This updates Apollo MCP Server’s tool schemas from [Draft 2020-12](https://json-schema.org/draft/2020-12) to [Draft‑07](https://json-schema.org/draft-07) which is more widely supported across different validators. VSCode/Copilot still validate against Draft‑07, so rejects Apollo MCP Server’s tools. Our JSON schemas don’t rely on newer features, so downgrading improves compatibility across MCP clients with no practical impact.

## 🛠 Maintenance

### Update rmcp sdk to version 0.8.x - @swcollard PR #433 

Bumping the Rust MCP SDK version used in this server up to 0.8.x

### chore: Only initialize a single HTTP client for graphql requests - @swcollard PR #412

Currently the MCP Server spins up a new HTTP client every time it wants to make a request to the downstream graphql endpoint. This change creates a static reqwest client that gets initialized using LazyLock and reused on each graphql request.

This change is based on the suggestion from the reqwest [documentation](https://docs.rs/reqwest/latest/reqwest/struct.Client.html)
> "The Client holds a connection pool internally, so it is advised that you create one and reuse it."

