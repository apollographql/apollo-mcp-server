---
default: patch
---

# Don't require `@include`/`@skip`-gated fields in `outputSchema`

`outputSchema` listed every non-null selected field in `required`, including
fields gated by `@include(if:)` or `@skip(if:)` on the field, an inline
fragment, or a fragment spread. GraphQL omits those fields from the response
when the condition excludes them, so clients that validate tool results
against `outputSchema` rejected valid responses. Such fields are now left out
of `required` unless the same response key is also selected without a
condition. When a gated selection shares its response key with another
selection, its nested fields are not required either. Literal
`@include(if: true)` and `@skip(if: false)` still count as unconditional.
Field types are unchanged.
