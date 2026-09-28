---
default: patch
---

# Use `Mcp-Name` headers for tool exceptions and per-operation scopes

For SDK-known protocol versions from `2026-07-28` onward, the auth middleware now reads the `tools/call` tool name from `Mcp-Name` instead of the body, for both `skip_token_validation.tools` and `overrides.required_scopes`. Authenticated tool calls no longer buffer the body in the auth middleware when their headers are usable. A tokenless request with a missing or malformed `Mcp-Method` or `Mcp-Name` is rejected. Older protocol versions keep the 16 KiB body peek.

Also updates `rmcp` to 3.5.0, which rejects repeated SEP-2243 headers and validates `Mcp-Method` on `initialize`, so the server's own checks for both are removed.
