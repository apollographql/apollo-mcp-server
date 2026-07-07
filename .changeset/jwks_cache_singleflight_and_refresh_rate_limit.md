---
default: minor
---

# Per-issuer JWKS cache with singleflight fetches and refresh rate limiting

Apollo MCP Server now manages JSON Web Key Set (JWKS) as a cached, per-issuer resource instead of refetching keys for every token validation. Previously, each request triggered two network calls—OpenID Connect (OIDC) discovery and JWKS fetch—regardless of whether the issuer had been seen before. The new cache reuses JWKS responses on the warm path and triggers a refresh only when the entry is missing, past its TTL, or does not contain the requested key ID.

The cache TTL is 10 minutes. Stale entries continue to serve known key IDs while a refresh is in progress or when a refresh fails — keys are only evicted when a successful refresh no longer includes them.

When multiple requests arrive simultaneously for an issuer whose JWKS is not yet cached (or whose entry has just expired), Apollo MCP Server now coalesces them into a single outbound discovery + JWKS fetch instead of fanning out one fetch per request. Followers await the leader's result and then read the populated cache; one upstream round-trip serves all concurrent callers.

Apollo MCP Server now allows at most one JWKS refresh per issuer per internal window (default 60 seconds). When a token arrives with a `kid` that is absent from both the fresh and stale cached JWKS and the window is exhausted, the request is rejected with 401 immediately — no outbound HTTP is made. Concurrent misses within an allowed window still coalesce into a single upstream fetch.

This closes the request-amplification vector where an attacker rotating unique bogus `kid`s could drive one upstream discovery + JWKS fetch pair per request.
