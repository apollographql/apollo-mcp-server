---
default: minor
---

# Resolve signing algorithm when a JWK omits `alg`

Apollo MCP Server now infers the signing algorithm from the authorization server's discovery metadata when a JWK omits the `alg` field, enabling support for providers like Azure AD B2C, Microsoft Entra ID, and AWS Cognito.
