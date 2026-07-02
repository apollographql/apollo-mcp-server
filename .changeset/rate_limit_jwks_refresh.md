---
default: minor
---

# Rate-limit JWKS refreshes per issuer

Apollo MCP Server now allows at most one JWKS refresh per issuer per configurable window (`jwks_min_refresh_interval`, default 60 seconds). When a token arrives with a `kid` that is not in the cached JWKS and the window is exhausted, the request is rejected with 401 immediately — no outbound HTTP is made. Concurrent misses within an allowed refresh still coalesce into a single upstream fetch.

This closes the request-amplification vector where an attacker rotating unique bogus `kid`s could drive one upstream discovery + JWKS fetch pair per request.
