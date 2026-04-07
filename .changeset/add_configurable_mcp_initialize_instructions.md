---
default: patch
---

# Add optional `instructions` for MCP initialize

Operators can set a top-level `instructions` value in the server configuration, or supply it through `APOLLO_MCP_INSTRUCTIONS`, to describe how models should use this server's tools and resources. The server returns that string in the MCP `initialize` response so clients can surface it to the model, consistent with the protocol's optional instructions field.
