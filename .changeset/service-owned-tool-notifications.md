---
default: patch
---

Replace shared MCP peer tracking with service-owned tool-change notification tasks. Slow clients no longer delay catalog updates or other clients, and legacy session teardown releases notification resources. Existing sessionless HTTP behavior and supported protocol versions are preserved.
