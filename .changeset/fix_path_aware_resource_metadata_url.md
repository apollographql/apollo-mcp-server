 ---
default: patch
---

# Fix path-aware OAuth protected resource metadata URL per RFC 9728

The `resource_metadata` URL in the `WWW-Authenticate` header and the `.well-known` endpoint route were ignoring the path component of the configured `resource` URL. Per [RFC 9728 Section 3](https://datatracker.ietf.org/doc/html/rfc9728#name-obtaining-protected-resourc), the well-known URI must be formed by inserting `/.well-known/oauth-protected-resource` between the host and path components. This fixes OAuth metadata discovery for MCP servers deployed behind reverse proxies with path-based routing, where clients like VS Code and Claude could not authenticate.
