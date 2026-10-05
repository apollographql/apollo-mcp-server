---
default: minor
---

# Load provided schemas that only break newer validation rules, and add `schema.validation`

A schema that isn't a supergraph now loads with a warning when it breaks only rules introduced by the GraphQL September 2025 specification, such as a deprecated field that implements a non-deprecated interface field. Other validation errors still prevent startup. Since v1.20.0, published schemas such as GitHub's failed to load for this reason.

The new `schema.validation` option controls this. `strict` (the default) behaves as described above, and `lenient` loads a schema with any validation error and logs every error as a warning. Parse errors prevent startup with either value.

```yaml
schema:
  source: local
  path: ./schema.graphql
  validation: lenient
```
