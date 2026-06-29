---
default: patch
---

# Gate JWKS fetches on unverified iss/aud claims

When JWT authentication is configured, the server previously attempted to fetch JWKS keys before validating issuer and audience claims, meaning every inbound token—even those from untrusted issuers—would trigger a key-resolution network call.

Apollo MCP Server now decodes the JWT payload before calling `KeyResolver::resolve_key` and rejects tokens whose `iss` is not in the configured issuer allowlist or whose `aud` does not overlap with the configured audiences. This avoids unnecessary JWKS fetches for tokens that could never validate. The pre-check is strictly an early-exit: every token rejected at this stage would also be rejected after signature verification, so the unverified payload is only ever used to drop tokens, never to authorize them.
