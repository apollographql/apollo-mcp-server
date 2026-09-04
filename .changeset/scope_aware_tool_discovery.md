---
default: minor
---

# Filter tool discovery by caller OAuth scopes

Add the opt-in `transport.auth.filter_tools_by_scope` setting to filter
`tools/list` using the per-tool requirements configured under
`overrides.required_scopes`. Call-time scope enforcement remains unchanged.
