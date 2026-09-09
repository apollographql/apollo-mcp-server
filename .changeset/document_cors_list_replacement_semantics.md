---
default: patch
---

# Correct the documented CORS defaults and document list replacement

The CORS documentation showed `allow_methods` defaulting to `GET, POST`. The actual default has been `GET, POST, DELETE` for some time, and DELETE is how a client terminates a session it no longer needs, so a reader who copied the documented block as a starting point lost session termination.

The documentation also never said that `allow_methods`, `allow_headers`, and `expose_headers` replace the default lists rather than extend them. An operator setting `allow_headers` to add one custom header silently dropped `mcp-protocol-version` and `mcp-session-id` from the preflight response, which breaks every browser-based MCP client with nothing in the config or the logs pointing at the cause. The auth documentation steers operators into exactly this edit when they use `skip_token_validation.headers` with browser clients, so it now links to a worked example that repeats the defaults alongside the added header.
