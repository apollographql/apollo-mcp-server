---
default: minor
---

# Add per-issuer JWKS cache

Apollo MCP Server now manages JWKS as a cached, per-issuer resource instead of refetching keys for every token validation. Previously, each request triggered two network calls (OIDC discovery and JWKS fetch) regardless of whether the issuer had been seen before. The new cache reuses JWKS responses on the warm path and falls back to a full fetch only when the cache is missing, stale, or does not contain the requested key ID.

The cache TTL defaults to 10 minutes and can be tuned via `transport.auth.jwks_cache_ttl` in YAML config or the corresponding environment variable.
