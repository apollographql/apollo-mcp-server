---
default: patch
---

# Tolerate transient Uplink manifest fetch errors during startup

A transient Uplink persisted-query manifest fetch error (network timeout, DNS failure, HTTP 5xx, or a retryable `retry_later` response) is now retried instead of being treated as fatal. Previously the first such error during startup would exit the server, even though the Uplink poll loop would have recovered on the next tick.

Apollo MCP Server now distinguishes transient errors (logged at warn and retried by the poll loop) from non-retryable errors such as an invalid API key (which remain fatal during startup and surface as an error while running). This matches how operation collections already behave, and the keep-last-good handling added previously is unchanged.
