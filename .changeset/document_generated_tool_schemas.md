---
default: patch
---

# Document how GraphQL types map to generated tool schemas

The Define MCP Tools page gains a "Generated tool schemas" reference: a worked example with the exact `inputSchema` the server emits, the GraphQL-to-JSON-Schema type mapping, how nullability and `required` are expressed, how default values are converted, where input property descriptions come from, and the shape of the optional `outputSchema`. No runtime behavior changes.
