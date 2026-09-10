---
default: minor
---

# Prepare configurable cache hints for MCP list and read responses

Add `caching.ttl_ms` (default: 300,000 milliseconds) to configure SEP-2549 cache hints for `tools/list`, `resources/list`, `resources/read`, and `prompts/list`. Remote app resource reads omit cache hints. Cache scope is always `private` when hints are provided because response visibility can depend on authentication scope.

Hints require negotiated MCP protocol version `2026-07-28` or newer. The server currently caps negotiation at `2025-11-25`, so this is preparatory support: clients will not receive cache hints until the server implements and negotiates the required protocol revision.
