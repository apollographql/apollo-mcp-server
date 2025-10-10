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