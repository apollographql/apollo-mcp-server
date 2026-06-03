---
default: minor
---

# Add issuer validation for OAuth tokens

Apollo MCP Server can now validate the `iss` (issuer) claim of incoming JWTs. Issuer
validation is configured per authorization server: a `transport.auth.servers` entry may
now be written as an object with a `url` and an optional `issuers` list, in addition to
the existing bare URL string form. When a server lists `issuers`, a token whose signature
is verified by that server's keys must carry an `iss` claim matching one of the listed
values, or the request is rejected. Binding issuers to the signing server (rather than a
single global list) mirrors Apollo Router's per-JWKS `issuers` semantics and prevents a
token signed by one configured server from being accepted while claiming a different
configured server's issuer. Server entries written as bare URL strings skip issuer
validation, so existing configurations are unaffected.
