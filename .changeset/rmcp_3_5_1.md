---
default: patch
---

# Update rmcp to 3.5.1

On protocol `2026-07-28`, errors such as an unknown tool, an unknown prompt, a missing prompt argument, or a missing resource are now sent with HTTP 200 as in-band JSON-RPC `-32602` errors. Previously they were sent with HTTP 400, which a client could mistake for a legacy server and fall back to `initialize`. Requests with malformed `_meta` are still rejected with HTTP 400.

Remote app resource reads now include `ttlMs: 0` and `cacheScope: "private"` for `2026-07-28` clients instead of omitting cache hints, which the protocol requires on every `resources/read` result.
