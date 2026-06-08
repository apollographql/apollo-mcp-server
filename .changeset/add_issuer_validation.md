---
default: minor
---

# Add issuer validation for OAuth tokens

Apollo MCP Server can now validate the `iss` (issuer) claim of incoming JWTs. Set
`transport.auth.issuers` to a list of accepted issuer values; a token's `iss` claim must
match one of them or the request is rejected. Issuer validation is also bound to the
authorization server that signed the token: the `iss` claim must equal the issuer that
server advertises in its discovery metadata, so a token signed by one configured server
cannot be accepted while it claims a different configured server's issuer. When `issuers`
is empty (the default), issuer validation is skipped, so existing configurations are
unaffected.
