---
default: patch
---

# Parse stringified JSON for list and input object arguments

Some MCP clients send a list or input object argument as a JSON string
instead of structured JSON, and the GraphQL server rejects the request. The
server now parses such a string when the variable has a list or input object
type and the string holds a JSON object or array. Strings for other variable
types, invalid JSON, and JSON scalars are forwarded unchanged.
