---
default: minor
---

# Singleflight JWKS fetches on cold cache misses

When multiple requests arrive simultaneously for an issuer whose JWKS is not yet cached (or whose entry has just expired), Apollo MCP Server now coalesces them into a single outbound discovery + JWKS fetch instead of fanning out one fetch per request. Followers await the leader's result and then read the populated cache; one upstream round-trip serves all concurrent callers.

This bounds the cold-start and TTL-flip fanout to a single upstream pair per issuer regardless of inbound concurrency.
