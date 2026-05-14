---
default: patch
---

# Preserve raw authorization server URLs in protected-resource metadata

Apollo MCP Server no longer normalizes the `transport.auth.servers` entries when it sets the `authorization_servers` field in `/.well-known/oauth-protected-resource`. Before, a scheme-authority-only configuration value, like `https://auth.example.com`, was re-parsed through `url::Url`, which added a trailing `/` to the empty path. This normalized form ended up in the metadata and caused mismatches in issuer claims for strict OAuth clients that compare `authorization_servers` with the auth server's discovery `issuer`. Now, server URLs are passed through exactly as they are, so users need to make sure each entry matches their auth server's `issuer` precisely.
