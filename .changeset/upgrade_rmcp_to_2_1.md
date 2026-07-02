---
default: minor
---

# Upgrade rmcp to 2.1 and support MCP 2025-11-25

Apollo MCP Server now depends on rmcp 2.1, which aligns the protocol implementation with the MCP 2025-11-25 specification. The server advertises `2025-11-25` as its latest supported protocol version and negotiates it with clients that request it. Clients on `2025-06-18` and `2025-03-26` continue to work, and clients requesting a version the server does not implement are downgraded to the latest supported version rather than refused. Output schema and structured content remain gated at `2025-06-18` and later, so they now apply to `2025-11-25` as well.

You can now configure browser Origin validation for the `streamable_http` transport with a new `allowed_origins` list under `host_validation`. Origin validation follows RFC 6454 and stays disabled when the list is empty, so existing configurations are unchanged.
