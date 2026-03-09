---
default: minor
---

# Implement Step-up Authorization Flow

Implements the [step-up authorization flow](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization#step-up-authorization-flow) from the MCP specification: when a client presents a valid token that lacks the scopes required for a specific operation, the server responds with HTTP 403 and a `WWW-Authenticate: Bearer error="insufficient_scope", scope="..."` header. The client can use this signal to re-authorize with elevated scopes and retry the request.
