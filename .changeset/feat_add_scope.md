---
default: minor
---

### Add scope parameter to WWW-Authenticate header - @DaleSeo PR #523

Add support for optional `scope` parameter in the `WWW-Authenticate` header per [MCP Auth Spec 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization#protected-resource-metadata-discovery-requirements).

When returning 401 Unauthorized responses, the server now includes the configured scopes to guide clients on appropriate scopes to request during authorization.

This PR extends the `WwwAuthenticate::Bearer` variant with an optional scope field. When scopes are configured, they are space-separated and included in 401 responses. When no scopes are configured, the parameter is omitted.
