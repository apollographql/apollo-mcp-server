---
default: patch
---

# Fix protocol version negotiation when output schema is enabled

Enabling `overrides.enable_output_schema` previously forced the MCP `initialize` response to advertise protocol version `2025-11-25`. Over the `streamable_http` transport that version was returned to clients verbatim, so any client that did not support `2025-11-25` was refused with `Server's protocol version is not supported: 2025-11-25`. With the flag disabled the server advertised `2025-03-26` and connected normally.

The server now negotiates the protocol version per the MCP lifecycle spec, echoing the client's requested version when it is supported and otherwise responding with the latest version the server supports (`2025-06-18`). The negotiated version no longer depends on `enable_output_schema`. The `outputSchema` and `structuredContent` fields stay gated by the negotiated version (`2025-06-18` and later), so older clients keep connecting without those fields.
