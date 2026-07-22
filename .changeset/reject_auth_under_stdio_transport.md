---
default: patch
---

# Reject auth configuration under the stdio transport

An `auth` block nested under a `stdio` transport was parsed successfully and then silently dropped, so an operator could believe authentication was configured while the server ran with none of it applied. Apollo MCP Server now rejects misplaced keys such as `auth` under `stdio` at config parse time, matching how top-level `auth` and the `streamable_http` transport already reject unknown fields. Configurations that place `auth` under `streamable_http` (the normal case) are unaffected.
