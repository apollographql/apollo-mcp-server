---
default: patch
---

# Use method headers for anonymous discovery when the protocol validates them

Anonymous method matching now prefers `Mcp-Method` for SDK-known protocol versions from `2026-07-28` onward, avoiding auth middleware body buffering when the header decides access. Older versions and requests without that header retain the 16 KiB body peek. Tool-name exceptions still inspect the body. The server's advertised protocol versions are unchanged.

The deprecated `allow_anonymous_mcp_discovery` flag retains its existing method list; its documentation and deprecation guidance now also list the already-supported `server/discover`. A local initialization check rejects contradictory method headers to cover rmcp's handshake exemption.
