---
default: minor
---

# Add header transform support

Add a `HeaderTransform` callback that allows consumers to modify HTTP headers before they are sent to the upstream GraphQL endpoint. The callback runs after all other header processing (static headers, forwarded headers, auth token passthrough, and mcp-session-id), enabling custom authentication schemes, header-based routing, HMAC signing, and other transformations without requiring an intermediary proxy.
