---
default: minor
---

# Use operation and variable descriptions in tool definitions

Operations and their variables can now carry descriptions, as defined by the GraphQL September 2025 specification. An operation's description becomes the tool description, ahead of leading `#` comments; config-level `overrides.descriptions` still take priority over both. A variable's description becomes its input property description, ahead of a `#` comment on the variable and the schema's argument description. A variable description applies to the variable as a whole; describing individual fields of an input object isn't supported.

Descriptions on operations, fragments, and variables are removed before an operation is sent to the GraphQL endpoint, so endpoints whose parsers predate the syntax, including the Apollo Router, keep working.
