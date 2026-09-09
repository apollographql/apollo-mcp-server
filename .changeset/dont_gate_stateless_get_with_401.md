---
default: patch
---

# Don't reject an unauthenticated GET with 401 on a stateless streamable-HTTP transport

With `transport.auth` configured and `stateful_mode: false`, an unauthenticated `GET` on the MCP endpoint now returns 405, matching what it already returned once a credential skipped validation, instead of 401. That route was never served in stateless mode regardless of a credential, so the 401 protected nothing on the wire. It did have a client-visible effect: at least one MCP client reads a 401 on that GET as "this server requires authentication" and hides every tool, including tools `transport.auth.skip_token_validation` was configured to expose without a token.

A stateful transport's GET is unaffected: it still returns 401 when unauthenticated, because that GET carries the real server-to-client stream and must stay protected.
