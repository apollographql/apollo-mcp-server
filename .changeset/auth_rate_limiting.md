---
default: minor
---

# Rate limit the OAuth token validation path

When OAuth authentication is configured, the server now rate limits requests
before it validates the bearer token. Each bearer credential gets its own token
bucket (10 requests per second, burst of 50). Requests over the limit receive
`429 Too Many Requests` with `Retry-After: 1` before any JWT parsing or JWKS
lookup. Only requests that fail authentication count against the limit, so
clients sharing a valid token are not throttled outside of very large
concurrent bursts. Requests without a bearer credential and the OAuth discovery endpoints
under `/.well-known/` are not limited.

There are no new configuration fields and no per-IP limit. Behind a reverse
proxy or load balancer the connection address identifies the proxy rather than
the client, so a per-IP limit would make unrelated clients share one bucket. A
per-client limit that understands trusted proxies is planned as a follow-up.

Rejections are counted in the new `apollo.mcp.auth.rate_limit.count` metric and
capacity fail-open events in `apollo.mcp.auth.rate_limit.overflow.count`.
Repeated token-validation warnings are now summarised at most once every 60
seconds with a count of suppressed events, instead of one log line per failed
request. Suppressed events are still logged at `DEBUG`.
