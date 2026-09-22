---
default: minor
---

# Validate schemas with the GraphQL September 2025 rules

MCP Server now parses schemas with apollo-compiler 2.0, pinned to `2.0.0-beta.1`, which follows the GraphQL September 2025 specification. Schemas you provide directly are validated against its stricter rules, so one that loaded before can now be rejected at startup or on hot reload. For example, `@deprecated` on a required argument, a default value that doesn't match its type, or one object type used for more than one root operation is now an error. Federation already validates the API schema it derives from a supergraph, so rule violations there are logged as warnings instead of preventing the server from starting.
