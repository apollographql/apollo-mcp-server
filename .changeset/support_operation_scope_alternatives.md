---
default: minor
---

# Support per-operation scope alternatives

`overrides.required_scopes` now accepts nested scope lists for per-operation authorization. Flat lists keep their existing behavior and require every listed scope. Nested lists define alternatives: each inner list is an AND group, and the outer list is OR.

This lets a single operation accept scope rules such as either `user:write` plus `tenant:admin`, or `admin`, while preserving existing flat-list configurations and flat-map builder inputs.

When a per-operation `403` is returned for a nested requirement, the `WWW-Authenticate` header's `scope` auth-param always names the complete first-listed alternative, regardless of the presented token, and its `scope_mode` auth-param always reports `require_all` regardless of the globally configured `scope_mode`.

If you embed Apollo MCP Server as a library, `Server::builder()`'s `required_scopes` parameter now takes `impl Into<OperationScopeRequirements>` instead of `HashMap<String, Vec<String>>` directly. Existing `HashMap<String, Vec<String>>` call sites keep working unchanged through that conversion.
