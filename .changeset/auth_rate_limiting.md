---
default: minor
---

# Rate limit the OAuth token validation path

When OAuth authentication is configured, the server now rate limits requests
before it validates the bearer token. Each bearer credential gets its own token
bucket (10 requests per second, burst of 50), and each connection peer address
gets a higher safety bucket (500 requests per second, burst of 1,000). Requests
over either limit receive `429 Too Many Requests` with `Retry-After: 1` before
any JWT parsing or JWKS lookup. The OAuth discovery endpoints under
`/.well-known/` are not limited.

There are no new configuration fields. Forwarded client-IP headers such as
`X-Forwarded-For` are not used, so all traffic arriving through one reverse
proxy or load balancer shares the per-peer bucket, while distinct bearer
credentials keep separate buckets.

Rejections are counted in the new `apollo.mcp.auth.rate_limit.count` metric and
capacity fail-open events in `apollo.mcp.auth.rate_limit.overflow.count`, both
labelled with `rate_limit_kind` set to `credential` or `peer`. Repeated
token-validation warnings are now summarised at most once every 60 seconds with
a count of suppressed events, instead of one log line per failed request.
