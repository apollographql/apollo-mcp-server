---
default: minor
---

# Configure which requests skip OAuth token validation

`transport.auth.skip_token_validation` lists requests that skip bearer token validation, keyed on the JSON-RPC method name, the tool named by a `tools/call`, or an HTTP header name such as `x-api-key`. `tools/call`, `resources/read`, `resources/subscribe`, `resources/unsubscribe`, `prompts/get`, and `completion/complete` are rejected from the method list, since each names one of many items through a request parameter the list can't see; `authorization` is rejected from the header list, since it could never match. A `tools/call` carrying an `?app=` query parameter never matches the tool list either, because the server dispatches that app's own tool rather than the named operation. Every list applies only when the request carries no `Authorization` header, so a caller that presents a token is always validated and an expired token is rejected even on a listed method, tool, or header. A tokenless request with a body too large or malformed to inspect no longer fails outright on that alone: it still gets the normal 401 challenge instead of a 413 or 400, since it was never going to succeed without a token.

This lets a deployment keep public tools reachable without a token, and lets a non-OAuth credential such as an API key coexist with OAuth, authenticated by a later layer.

`allow_anonymous_mcp_discovery` is deprecated in favor of `skip_token_validation.methods`, which it now maps onto (`initialize`, `tools/list`, `resources/list`) with a startup warning. Setting both is a startup error.
