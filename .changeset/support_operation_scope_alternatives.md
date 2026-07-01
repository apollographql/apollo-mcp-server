---
default: patch
---

# Support per-operation scope alternatives

`overrides.required_scopes` now accepts nested scope lists for per-operation authorization. Flat lists keep their existing behavior and require every listed scope. Nested lists define alternatives: each inner list is an AND group, and the outer list is OR.

This lets a single operation accept scope rules such as either `user:write` plus `tenant:admin`, or `admin`, while preserving existing flat-list configurations and flat-map builder inputs.
