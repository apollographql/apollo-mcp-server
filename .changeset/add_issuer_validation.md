---
default: minor
---

# Add issuer validation for OAuth tokens

Apollo MCP Server can now validate the `iss` (issuer) claim of incoming JWTs. Set
`transport.auth.issuers` to a list of accepted issuer URLs; a token's `iss` claim must
match one of them or the request is rejected. When `issuers` is empty (the default),
issuer validation is skipped, so existing configurations are unaffected. This brings the
MCP Server to parity with Apollo Router's JWT issuer validation.
