---
default: patch
---

# Validate generated GraphQL tool schemas against JSON Schema 2020-12

Repeated non-null selections no longer create duplicate `required` entries.
Output schemas now validate fields selected through nested, named, and inline
fragments on unions and interfaces together, while accepting members with no
matching fragment and rejecting incompatible member-key combinations.
Regression tests check generated input and output schemas against Draft 2020-12
and through MCP `tools/list`.
