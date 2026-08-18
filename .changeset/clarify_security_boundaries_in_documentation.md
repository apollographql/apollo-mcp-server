---
default: patch
---

# Clarify security boundaries in documentation

The documentation now describes the server's security controls in terms of what the implementation actually enforces — secret handling, header forwarding, OAuth scope semantics, mutation modes, the GraphQL execution boundary, host and Origin validation, and telemetry redaction — along with their boundaries and the responsibilities that remain with MCP clients, operators, and upstream GraphQL services. Runtime behavior is unchanged apart from a more precise warning when global scope enforcement is disabled while global scopes are configured, plus a regression test covering explicitly forwarded authorization headers when automatic passthrough is disabled.
