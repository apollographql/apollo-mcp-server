---
default: patch
---

# Use method headers for anonymous discovery when the protocol validates them

Anonymous method matching now prefers `Mcp-Method` for SDK-known protocol versions from `2026-07-28` onward, avoiding auth middleware body buffering when the header decides access. Older versions and requests without that header retain the 16 KiB body peek. Tool-name exceptions still inspect the body. The server's advertised protocol versions are unchanged.

The deprecated `allow_anonymous_mcp_discovery` flag now permits `server/discover`, `tools/list`, and `resources/list`. Legacy clients that need anonymous initialization must explicitly list `initialize` in `skip_token_validation.methods`. A local initialization check rejects contradictory method headers to cover rmcp's handshake exemption.
