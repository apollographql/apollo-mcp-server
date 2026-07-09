---
default: minor
---

# Support authorization servers without a discovery endpoint

`transport.auth` now accepts `static_servers`, for authorization servers that don't expose an RFC 8414 or OIDC discovery endpoint. Each entry sets `issuer` and `jwks_uri` directly, so Apollo MCP Server fetches signing keys without a discovery request.

`static_servers` entries behave the same as `servers` entries for caching, rate limiting, and issuer binding, and the two lists can be used together. Existing `servers` configurations are unaffected.
