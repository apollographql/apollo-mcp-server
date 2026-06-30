---
default: patch
---

# Add per-issuer JWKS cache to eliminate redundant network calls

Each token validation previously triggered two network calls (discovery + JWKS fetch) regardless of whether the issuer had been seen before. Apollo MCP Server now caches JWKS responses per issuer and reuses them on the warm path, falling back to a full fetch only when the cache is missing, stale, or does not contain the requested key ID.

The cache TTL defaults to 10 minutes and can be tuned via `jwks_cache_ttl` in YAML config or the corresponding environment variable.
