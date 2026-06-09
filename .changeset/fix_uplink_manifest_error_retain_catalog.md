---
default: patch
---

# Retain tool catalog on transient Uplink manifest fetch failure

When `operations.source: uplink` is configured, a transient Uplink persisted-query manifest fetch failure (network timeout, DNS failure, HTTP 5xx, or a retryable `retry_later` response) previously caused the server to silently replace its active tool catalog with an empty list. The server continued reporting `/health` as UP and sent `tools/list_changed` to clients, which then received `{"tools": []}` with no error signal.

Apollo MCP Server now retains the last known good tool catalog when a transient Uplink manifest fetch error occurs and logs the error. The catalog is only cleared when Uplink authoritatively returns a valid empty manifest (HTTP 200 with an empty persisted query collection), which represents an intentional operator action. A manifest fetch failure during initial startup before the first successful load remains fatal.
