---
default: patch
---

# Apply variable defaults when a tool argument is null

A variable with a default value is exposed as an optional, nullable tool
argument. When a client sent an explicit `null` for such an argument, the
`null` was forwarded and overrode the default. For a `Boolean` used in
`@include(if:)` or `@skip(if:)`, this made the GraphQL server reject the
request. The server now omits `null` arguments for variables that declare a
default, so the default applies. `null` arguments for variables without a
default are still forwarded unchanged.
