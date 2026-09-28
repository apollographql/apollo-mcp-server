---
default: patch
---

# Support MCP protocol 2026-07-28

Declare support for MCP 2026-07-28, enabling subscriptions/listen, cache hints,
and standard request headers. Modern responses include resultType: "complete";
missing resources return -32602 and unknown methods return HTTP 404 with -32601.
Older supported protocol versions remain available.
