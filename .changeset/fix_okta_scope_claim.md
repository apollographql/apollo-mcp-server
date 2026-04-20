---
default: patch
---

# Accept the `scp` JWT claim for OAuth scope validation

Previously, the MCP server only read the RFC 9068 `scope` claim when validating OAuth tokens. Okta emits scopes as the non-standard `scp` claim, which caused otherwise-valid tokens from those providers to be rejected as having insufficient scopes. The server now falls back to `scp` when `scope` is absent.
