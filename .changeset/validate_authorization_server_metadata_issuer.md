---
default: patch
---

# Validate authorization server metadata issuer

Apollo MCP Server now checks that the `issuer` in an authorization server's discovery metadata matches the server it was fetched from, as required by [RFC 8414 section 3.3](https://datatracker.ietf.org/doc/html/rfc8414#section-3.3). Metadata that advertises a mismatched issuer is rejected before its keys are trusted, so a token cannot be bound to an issuer identity the signing server did not actually claim. Servers whose discovery document returns a matching issuer (the normal case) are unaffected.
