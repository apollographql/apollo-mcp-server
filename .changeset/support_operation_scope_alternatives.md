---
default: minor
---

# Support per-operation scope alternatives

`overrides.required_scopes` now accepts nested scope lists for per-operation authorization. Flat lists keep their existing behavior and require every listed scope. Nested lists define alternatives: each inner list is an AND group, and the outer list is OR.

This lets a single operation accept scope rules such as either `user:write` plus `tenant:admin`, or `admin`, while preserving existing flat-list configurations and flat-map builder inputs.

If you embed Apollo MCP Server as a library, `Server::builder()`'s `required_scopes` parameter now takes `impl Into<OperationScopeRequirements>` instead of `HashMap<String, Vec<String>>` directly. Existing `HashMap<String, Vec<String>>` call sites keep working unchanged through that conversion.
