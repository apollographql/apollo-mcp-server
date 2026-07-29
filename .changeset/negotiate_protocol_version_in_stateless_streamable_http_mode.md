---
default: patch
---

# Negotiate protocol version in stateless streamable HTTP mode

When running streamable HTTP in stateless mode, the server answered `initialize` with its latest supported MCP protocol version regardless of the version the client requested, breaking clients that require an older version (such as AWS AgentCore requesting `2025-06-18`). The server now echoes the client's requested protocol version when it is supported and falls back to the latest supported version otherwise, matching the negotiation behavior of the stdio and stateful HTTP transports.
