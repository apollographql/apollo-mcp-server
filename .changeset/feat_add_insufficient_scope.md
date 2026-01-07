### feat(auth): Add 403 Forbidden `insufficient_scope` support per MCP Auth Spec 2025-11-25 and RFC 6750 (Section 3.1) - @gocamille PR #537

<!-- https://apollographql.atlassian.net/browse/AMS-171 -->

## Summary
This adds HTTP 403 Forbidden responses with `error="insufficient_scope"` per [MCP Auth Spec 2025-11-25 Section 10: Error Handling](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization#error-handling) and [RFC 6750 Section 3.1](https://www.rfc-editor.org/rfc/rfc6750.html#section-3.1).

## Changes
- `www_authenticate.rs`: Added `BearerError::InsufficientScope` enum and [error](cci:1://file:///Users/camillelawrence/Desktop/repos/apollo-mcp-server/crates/apollo-mcp-server/src/auth/www_authenticate.rs:135:4-149:5) field to `WWW-Authenticate` header
- `valid_token.rs`: Extract [scope](cci:1://file:///Users/camillelawrence/Desktop/repos/apollo-mcp-server/crates/apollo-mcp-server/src/auth/valid_token.rs:27:0-38:1)/[scp](cci:1://file:///Users/camillelawrence/Desktop/repos/apollo-mcp-server/crates/apollo-mcp-server/src/auth/valid_token.rs:503:8-509:9) claims from JWTs (handles both standard OAuth and Azure AD)
- `auth.rs`: Scope validation with fail-closed behaviour—valid tokens lacking required scopes get `403`
- `headers.rs`: Updated tests for new `ValidToken` struct

## Behavior
- **401 Unauthorized**: Missing or invalid token
- **403 Forbidden**: Valid token but insufficient scopes (includes `error="insufficient_scope"` in response)
