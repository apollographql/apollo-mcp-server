---
default: patch
---

# Keep generated output schemas valid for repeated GraphQL selections

Repeated non-null field selections (for example, `node { id id }`) no longer
produce duplicate entries in JSON Schema's `required` array. Duplicate entries
made the generated output schema invalid even though the GraphQL operation was valid.

Regression coverage validates generated tool schemas against JSON Schema 2020-12,
including nested inputs, unions, interfaces, and custom scalar compositions, and
checks schemas retrieved through MCP `tools/list`. GraphQL output schemas continue
to describe the object response envelope; array and primitive output roots are
covered separately as SDK serialization compatibility checks.
