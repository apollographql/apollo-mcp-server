---
default: minor
---

# Support the `server/discover` RPC

The server now answers `server/discover`, the RPC MCP 2026-07-28 requires for up-front protocol version selection. Clients can call it before any other request — including as the opening message on stdio, where it doubles as a backward-compatibility probe — without first establishing a session.

The response advertises the same capabilities the `initialize` response returns, lists the protocol versions the server implements, and carries the server's implementation metadata — name, version, title, website URL, and description — sourced from the `server_info` configuration key. Note that discovery carries this metadata in the result's `_meta` under `io.modelcontextprotocol/serverInfo` rather than a top-level field.

`server/discover` can now be listed in `skip_token_validation.methods` to allow it without authentication, and the deprecated `allow_anonymous_mcp_discovery` flag now folds it in alongside `initialize`, `tools/list`, and `resources/list`, since `server/discover` may be a client's first MCP request.

The newest protocol version the server negotiates remains `2025-11-25`. Answering `server/discover` does not by itself make the server conformant with `2026-07-28`: that revision also replaces the standalone notification stream with `subscriptions/listen`, which this server does not yet implement. Advertising `2026-07-28` while the server announces `tools.listChanged` would promise change notifications it has no way to deliver, so the supported-version list stays capped until that work lands.
