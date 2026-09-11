---
default: patch
---

### Prepare tool-change subscription streams

Add request-owned tool-change notification delivery for MCP `subscriptions/listen`, including explicit client opt-in, an initial catalog refresh, and cleanup on cancellation or disconnect. This prepares for a future MCP 2026-07-28 rollout; the supported protocol version remains capped at 2025-11-25, and existing legacy notifications and HTTP session behavior are preserved.
