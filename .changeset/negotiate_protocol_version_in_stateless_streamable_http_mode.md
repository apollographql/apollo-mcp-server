---
default: patch
---

# Negotiate protocol version in stateless streamable HTTP mode

When running streamable HTTP in stateless mode, the server answered `initialize` with its newest supported MCP protocol version regardless of the version the client requested, breaking clients that require an older version (such as AWS AgentCore requesting `2025-06-18`). The server now echoes the client's requested protocol version when it is a version the server implements, and otherwise negotiates down to the newest version the server implements (`2025-11-25`). Versions that the underlying SDK recognizes but this server does not yet implement are capped rather than advertised, so the server never claims support for a revision it can't serve.
