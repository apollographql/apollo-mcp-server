---
default: minor
---

# Use operation descriptions as tool descriptions

Operations can now carry a description, as defined by the GraphQL September 2025 specification. MCP Server uses it as the tool description, ahead of leading `#` comments; config-level `overrides.descriptions` still take priority over both.

Descriptions on operations, fragments, and variables are removed before an operation is sent to the GraphQL endpoint, so endpoints whose parsers predate the syntax, including the Apollo Router, keep working.
